//! A small HTTP/1.1 client for talking to a model server on this machine.
//!
//! Oracle only ever speaks HTTP to a local inference server, so there is no
//! TLS here and no dependency that would bring it in. That is deliberate in
//! both directions: it keeps the binary small and pure-Rust, and it means the
//! client physically cannot reach an `https://` endpoint on the internet. A
//! remote endpoint is a policy decision the config gates (`allow_remote_endpoint`),
//! not something a typo can cause.
//!
//! Chunked transfer-encoding is decoded here because both Ollama and
//! llama.cpp stream their tokens that way.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug)]
pub enum HttpError {
    /// The endpoint string is not a URL Oracle can use.
    BadUrl(String),
    /// Nothing is listening, or the connection failed mid-flight.
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
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    pub fn parse(raw: &str) -> Result<Url, HttpError> {
        let raw = raw.trim();
        let rest = if let Some(r) = raw.strip_prefix("http://") {
            r
        } else if raw.starts_with("https://") {
            return Err(HttpError::BadUrl(
                "https is not supported; Oracle talks to a model server on this machine over http"
                    .into(),
            ));
        } else if raw.contains("://") {
            return Err(HttpError::BadUrl(format!("unsupported scheme in {raw}")));
        } else {
            // A bare `127.0.0.1:11434` is what people type; accept it.
            raw
        };

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
            (host.to_string(), port.unwrap_or(80))
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (
                    h.to_string(),
                    p.parse::<u16>()
                        .map_err(|_| HttpError::BadUrl(format!("bad port in {raw}")))?,
                ),
                None => (authority.to_string(), 80),
            }
        };

        Ok(Url {
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
            host: self.host.clone(),
            port: self.port,
            path: format!("{base}{path}"),
        }
    }

    pub fn origin(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "http://{}:{}{}", self.host, self.port, self.path)
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

/// The body of a response, decoding chunked encoding transparently.
enum Body {
    /// Exactly `remaining` more bytes, then EOF.
    Sized {
        reader: BufReader<TcpStream>,
        remaining: u64,
    },
    /// Chunked: a hex length line, that many bytes, CRLF, repeat until 0.
    Chunked {
        reader: BufReader<TcpStream>,
        remaining: usize,
        done: bool,
    },
    /// No framing at all; read until the peer closes.
    Eof { reader: BufReader<TcpStream> },
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

fn connect(url: &Url, timeout: Duration) -> Result<TcpStream, HttpError> {
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

fn send(
    method: &str,
    url: &Url,
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<Response, HttpError> {
    let mut stream = connect(url, timeout)?;

    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: oracle/{}\r\nAccept: application/json\r\nConnection: close\r\n",
        url.path,
        url.host,
        url.port,
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
            (u.host.as_str(), u.port, u.path.as_str()),
            ("127.0.0.1", 11434, "/")
        );

        let u = Url::parse("127.0.0.1:8080/v1").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.path.as_str()),
            ("127.0.0.1", 8080, "/v1")
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
    fn refuses_https_with_an_explanation() {
        let e = Url::parse("https://api.example.com").unwrap_err();
        assert!(matches!(e, HttpError::BadUrl(_)));
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
    }
}
