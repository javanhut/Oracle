//! A small HTTP/1.1 client for talking to a model server.
//!
//! Plain HTTP for a server on this machine, which is the usual case, and HTTPS
//! -- through rustls, verified against the system's trusted certificates --
//! for one behind a TLS proxy or on another machine. Certificates are always
//! verified. There is no switch to skip that, because an unverified TLS
//! connection carrying a description of your system is worse than a plain one
//! that at least does not pretend to be private.
//!
//! Reaching anywhere other than this machine is still a policy decision the
//! config gates (`allow_remote_endpoint`), not something a typo can cause:
//! `https://` changes how the bytes travel, not where Oracle is allowed to
//! send them.
//!
//! Chunked transfer-encoding is decoded here because both Ollama and
//! llama.cpp stream their tokens that way.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

#[derive(Debug)]
pub enum HttpError {
    /// The endpoint string is not a URL Oracle can use.
    BadUrl(String),
    /// Nothing is listening, the connection failed mid-flight, or TLS could
    /// not be established.
    Connect(String),
    /// The server answered, but not with something we can parse.
    Protocol(String),
    /// The server answered with a non-2xx status.
    Status(u16, String),
    Io(std::io::Error),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::BadUrl(s) => write!(f, "bad endpoint URL: {s}"),
            HttpError::Connect(s) => write!(f, "could not reach the model server: {s}"),
            HttpError::Protocol(s) => write!(f, "unexpected response: {s}"),
            HttpError::Status(c, b) => {
                let body = b.trim();
                if body.is_empty() {
                    write!(f, "server returned HTTP {c}")
                } else {
                    write!(f, "server returned HTTP {c}: {body}")
                }
            }
            HttpError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HttpError {}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> Self {
        HttpError::Io(e)
    }
}

/// A parsed endpoint. Splitting this out lets the caller ask whether the
/// target is loopback before any bytes are sent.
#[derive(Debug, Clone)]
pub struct Url {
    /// `https://`: the connection is TLS.
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    pub fn parse(raw: &str) -> Result<Url, HttpError> {
        let raw = raw.trim();
        let lower = raw.to_ascii_lowercase();
        let (tls, rest) = if lower.starts_with("https://") {
            (true, &raw["https://".len()..])
        } else if lower.starts_with("http://") {
            (false, &raw["http://".len()..])
        } else if raw.contains("://") {
            return Err(HttpError::BadUrl(format!(
                "unsupported scheme in {raw}; use http:// or https://"
            )));
        } else {
            // A bare `127.0.0.1:11434` is what people type; accept it.
            (false, raw)
        };
        let default_port = if tls { 443 } else { 80 };

        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(HttpError::BadUrl(format!("no host in {raw}")));
        }

        // Bracketed IPv6 literal, e.g. [::1]:11434
        let (host, port) = if let Some(close) = authority.find(']') {
            let host = authority[..close + 1]
                .trim_start_matches('[')
                .trim_end_matches(']');
            let port = authority[close + 1..]
                .strip_prefix(':')
                .map(|p| p.parse::<u16>())
                .transpose()
                .map_err(|_| HttpError::BadUrl(format!("bad port in {raw}")))?;
            (host.to_string(), port.unwrap_or(default_port))
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (
                    h.to_string(),
                    p.parse::<u16>()
                        .map_err(|_| HttpError::BadUrl(format!("bad port in {raw}")))?,
                ),
                None => (authority.to_string(), default_port),
            }
        };

        Ok(Url {
            tls,
            host,
            port,
            path: path.to_string(),
        })
    }

    /// Whether this endpoint stays on the machine.
    ///
    /// Resolution is included: a hostname that resolves off-box is not
    /// loopback however friendly it looks. A name that does not resolve at all
    /// is reported as not-loopback, so the cautious path is the default.
    pub fn is_loopback(&self) -> bool {
        if self.host == "localhost" {
            return true;
        }
        match (self.host.as_str(), self.port).to_socket_addrs() {
            Ok(addrs) => {
                let mut any = false;
                for a in addrs {
                    any = true;
                    if !a.ip().is_loopback() {
                        return false;
                    }
                }
                any
            }
            Err(_) => false,
        }
    }

    pub fn join(&self, path: &str) -> Url {
        let base = self.path.trim_end_matches('/');
        Url {
            tls: self.tls,
            host: self.host.clone(),
            port: self.port,
            path: format!("{base}{path}"),
        }
    }

    fn scheme(&self) -> &'static str {
        if self.tls { "https" } else { "http" }
    }

    /// The host as it appears in a URL, bracketed when it is an IPv6 literal.
    fn bracketed_host(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }

    pub fn origin(&self) -> String {
        format!(
            "{}://{}:{}",
            self.scheme(),
            self.bracketed_host(),
            self.port
        )
    }

    /// The `Host` header: the port is left off when it is the scheme's own.
    fn host_header(&self) -> String {
        let default = if self.tls { 443 } else { 80 };
        if self.port == default {
            self.bracketed_host()
        } else {
            format!("{}:{}", self.bracketed_host(), self.port)
        }
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.origin(), self.path)
    }
}

/// A response whose body has not been read yet.
///
/// Streaming replies are the normal case, so the body is exposed as a reader
/// rather than a `String`: the caller pulls lines out as the model produces
/// them and prints tokens as they arrive.
pub struct Response {
    body: Body,
}

impl Response {
    /// Read the whole body. Fine for small replies such as a model list.
    pub fn text(self) -> Result<String, HttpError> {
        let mut s = String::new();
        let mut body = self.body;
        body.read_to_string(&mut s)?;
        Ok(s)
    }

    /// Call `f` for each line of the body as it arrives.
    ///
    /// `f` returns `false` to stop early, which closes the connection and
    /// therefore tells the server to stop generating -- this is how Ctrl-C
    /// during a long answer avoids leaving the model running.
    pub fn for_each_line<F>(self, mut f: F) -> Result<(), HttpError>
    where
        F: FnMut(&str) -> bool,
    {
        let reader = BufReader::new(self.body);
        for line in reader.lines() {
            let line = line?;
            if !f(&line) {
                break;
            }
        }
        Ok(())
    }
}

/// A connection to the server, plain or TLS. Everything above this reads and
/// writes bytes and does not care which.
enum Conn {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Conn {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(s) => s.read(out),
            Conn::Tls(s) => match s.read(out) {
                // Plenty of servers close after `Connection: close` without a
                // TLS close_notify. The body's own framing -- a length, or a
                // final zero chunk -- already says whether it arrived whole,
                // so a bare close is treated as the end rather than an error.
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(0),
                other => other,
            },
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(s) => s.write(buf),
            Conn::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            Conn::Tls(s) => s.flush(),
        }
    }
}

/// The body of a response, decoding chunked encoding transparently.
enum Body {
    /// Exactly `remaining` more bytes, then EOF.
    Sized {
        reader: BufReader<Conn>,
        remaining: u64,
    },
    /// Chunked: a hex length line, that many bytes, CRLF, repeat until 0.
    Chunked {
        reader: BufReader<Conn>,
        remaining: usize,
        done: bool,
    },
    /// No framing at all; read until the peer closes.
    Eof { reader: BufReader<Conn> },
}

impl Read for Body {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Body::Sized { reader, remaining } => {
                if *remaining == 0 {
                    return Ok(0);
                }
                let want = out.len().min(*remaining as usize);
                let n = reader.read(&mut out[..want])?;
                *remaining -= n as u64;
                Ok(n)
            }
            Body::Eof { reader } => reader.read(out),
            Body::Chunked {
                reader,
                remaining,
                done,
            } => {
                if *done {
                    return Ok(0);
                }
                if *remaining == 0 {
                    // Between chunks: consume the CRLF that ended the last one
                    // (absent before the first), then read the size line.
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line)? == 0 {
                            *done = true;
                            return Ok(0);
                        }
                        if !line.trim().is_empty() {
                            break;
                        }
                    }
                    // A chunk size may carry `;ext=...` extensions.
                    let size_txt = line.trim().split(';').next().unwrap_or("").trim();
                    let size = usize::from_str_radix(size_txt, 16).map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("bad chunk size {size_txt:?}"),
                        )
                    })?;
                    if size == 0 {
                        *done = true;
                        return Ok(0);
                    }
                    *remaining = size;
                }
                let want = out.len().min(*remaining);
                let n = reader.read(&mut out[..want])?;
                *remaining -= n;
                Ok(n)
            }
        }
    }
}

fn connect_tcp(url: &Url, timeout: Duration) -> Result<TcpStream, HttpError> {
    let addrs = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(|e| HttpError::Connect(format!("cannot resolve {}: {e}", url.host)))?;

    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            Ok(s) => {
                s.set_read_timeout(Some(timeout))?;
                s.set_write_timeout(Some(Duration::from_secs(15)))?;
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(HttpError::Connect(match last {
        Some(e) => format!("{} ({e})", url.origin()),
        None => format!("{} has no address", url.host),
    }))
}

fn connect(url: &Url, timeout: Duration) -> Result<Conn, HttpError> {
    let sock = connect_tcp(url, timeout)?;
    if !url.tls {
        return Ok(Conn::Plain(sock));
    }

    let name = rustls::pki_types::ServerName::try_from(url.host.clone()).map_err(|_| {
        HttpError::BadUrl(format!(
            "{} is not a name a certificate can be for",
            url.host
        ))
    })?;
    let session = rustls::ClientConnection::new(tls_config()?, name)
        .map_err(|e| HttpError::Connect(format!("TLS with {} failed: {e}", url.origin())))?;
    let mut stream = rustls::StreamOwned::new(session, sock);

    // Finish the handshake now rather than on the first write, so a bad
    // certificate is reported as a TLS failure and not as a confusing error
    // halfway through sending a request.
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|e| HttpError::Connect(format!("TLS with {} failed: {e}", url.origin())))?;
    }
    Ok(Conn::Tls(Box::new(stream)))
}

/// The TLS settings, built once per process: rustls's safe defaults and the
/// certificates this machine trusts.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, HttpError> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(build_tls_config)
        .clone()
        .map_err(HttpError::Connect)
}

fn build_tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    let found = rustls_native_certs::load_native_certs();
    let mut roots = rustls::RootCertStore::empty();
    let (added, _ignored) = roots.add_parsable_certificates(found.certs);
    if added == 0 {
        return Err(
            "no trusted certificates were found on this machine, so no https server can be \
             verified. The ca-certificates package provides them."
                .into(),
        );
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("cannot set up TLS: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

fn send(
    method: &str,
    url: &Url,
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<Response, HttpError> {
    let mut stream = connect(url, timeout)?;

    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: oracle/{}\r\nAccept: application/json\r\nConnection: close\r\n",
        url.path,
        url.host_header(),
        env!("CARGO_PKG_VERSION"),
    );
    if let Some(b) = body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");

    stream.write_all(head.as_bytes())?;
    if let Some(b) = body {
        stream.write_all(b)?;
    }
    stream.flush()?;

    let mut reader = BufReader::new(stream);

    let mut status_line = String::new();
    if reader.read_line(&mut status_line)? == 0 {
        return Err(HttpError::Protocol(
            "the server closed without replying".into(),
        ));
    }
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| HttpError::Protocol(format!("bad status line {:?}", status_line.trim())))?;

    let mut content_length: Option<u64> = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == "content-length" {
                content_length = v.parse().ok();
            } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
        }
    }

    let body = if chunked {
        Body::Chunked {
            reader,
            remaining: 0,
            done: false,
        }
    } else if let Some(len) = content_length {
        Body::Sized {
            reader,
            remaining: len,
        }
    } else {
        Body::Eof { reader }
    };

    let resp = Response { body };

    if !(200..300).contains(&status) {
        // Error bodies are small and worth showing: a model server usually
        // explains itself ("model not found", "context too long").
        let text = resp.text().unwrap_or_default();
        let mut trimmed: String = text.chars().take(400).collect();
        if text.chars().count() > 400 {
            trimmed.push('…');
        }
        return Err(HttpError::Status(status, trimmed));
    }

    Ok(resp)
}

pub fn get(url: &Url, timeout: Duration) -> Result<Response, HttpError> {
    send("GET", url, None, timeout)
}

pub fn post_json(url: &Url, body: &[u8], timeout: Duration) -> Result<Response, HttpError> {
    send("POST", url, Some(body), timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_usual_endpoint_shapes() {
        let u = Url::parse("http://127.0.0.1:11434").unwrap();
        assert_eq!(
            (u.tls, u.host.as_str(), u.port, u.path.as_str()),
            (false, "127.0.0.1", 11434, "/")
        );

        let u = Url::parse("127.0.0.1:8080/v1").unwrap();
        assert_eq!(
            (u.tls, u.host.as_str(), u.port, u.path.as_str()),
            (false, "127.0.0.1", 8080, "/v1")
        );

        let u = Url::parse("http://localhost").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("localhost", 80));

        let u = Url::parse("http://[::1]:11434/api").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.path.as_str()),
            ("::1", 11434, "/api")
        );
    }

    #[test]
    fn https_is_tls_on_port_443_unless_a_port_is_given() {
        let u = Url::parse("https://models.example.com/v1").unwrap();
        assert_eq!(
            (u.tls, u.host.as_str(), u.port, u.path.as_str()),
            (true, "models.example.com", 443, "/v1")
        );
        let u = Url::parse("HTTPS://models.example.com:8443").unwrap();
        assert_eq!((u.tls, u.port), (true, 8443));
    }

    #[test]
    fn the_scheme_survives_into_every_way_a_url_is_shown() {
        let u = Url::parse("https://models.example.com/v1").unwrap();
        assert_eq!(u.origin(), "https://models.example.com:443");
        assert_eq!(
            u.join("/chat/completions").to_string(),
            "https://models.example.com:443/v1/chat/completions"
        );
        assert!(u.join("/x").tls, "joining a path must not drop TLS");
        assert_eq!(u.host_header(), "models.example.com");

        let plain = Url::parse("http://127.0.0.1:11434").unwrap();
        assert_eq!(plain.origin(), "http://127.0.0.1:11434");
        assert_eq!(plain.host_header(), "127.0.0.1:11434");
        assert_eq!(
            Url::parse("http://[::1]:11434").unwrap().host_header(),
            "[::1]:11434"
        );
    }

    #[test]
    fn an_unknown_scheme_is_refused_with_the_ones_that_work() {
        let e = Url::parse("ftp://api.example.com").unwrap_err();
        assert!(
            matches!(e, HttpError::BadUrl(ref s) if s.contains("https://")),
            "got {e}"
        );
    }

    #[test]
    fn tls_to_a_server_that_does_not_speak_it_fails_cleanly() {
        // A plain HTTP server answering a TLS handshake with HTTP. The client
        // must give up with a TLS error, promptly, rather than hang or panic.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        let url = Url::parse(&format!("https://127.0.0.1:{port}")).unwrap();
        let started = std::time::Instant::now();
        let e = match get(&url, Duration::from_secs(5)) {
            Ok(_) => panic!("a TLS handshake against plain HTTP must fail"),
            Err(e) => e,
        };
        assert!(
            matches!(e, HttpError::Connect(ref s) if s.contains("TLS")),
            "got {e}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "it must not wait for the timeout"
        );
    }

    #[test]
    fn join_does_not_double_the_slash() {
        let u = Url::parse("http://127.0.0.1:11434/").unwrap();
        assert_eq!(u.join("/api/tags").path, "/api/tags");
        let u = Url::parse("http://127.0.0.1:8080/v1").unwrap();
        assert_eq!(u.join("/chat/completions").path, "/v1/chat/completions");
    }

    #[test]
    fn loopback_is_recognised_by_address_not_by_name() {
        assert!(Url::parse("http://127.0.0.1:11434").unwrap().is_loopback());
        assert!(Url::parse("http://localhost:11434").unwrap().is_loopback());
        assert!(!Url::parse("http://192.0.2.10:11434").unwrap().is_loopback());
        assert!(
            !Url::parse("https://192.0.2.10").unwrap().is_loopback(),
            "https does not make a remote address local"
        );
    }
}
