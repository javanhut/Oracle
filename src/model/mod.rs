//! Talking to a local model.
//!
//! Two things matter here beyond the wire format.
//!
//! The first is that no model is a normal state, not an error. A fresh Raven
//! install has no inference server and no weights, and Oracle has to be
//! useful and quiet in that condition -- `oracle doctor` works, `oracle ask`
//! explains in one short paragraph what it would need and stops. It does not
//! offer to install anything on your behalf.
//!
//! The second is that the endpoint is on this machine. `Config::endpoint`
//! refuses a non-loopback address unless it has been explicitly permitted --
//! whether it is http or https, since TLS changes how the bytes travel and not
//! where they may go -- so "local" is enforced by the code rather than
//! promised by the documentation.

pub mod llamacpp;
pub mod ollama;

use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    System,
    User,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(c: impl Into<String>) -> Message {
        Message {
            role: Role::System,
            content: c.into(),
        }
    }
    pub fn user(c: impl Into<String>) -> Message {
        Message {
            role: Role::User,
            content: c.into(),
        }
    }
}

/// Why a model call could not happen or did not finish.
#[derive(Debug)]
pub enum ModelError {
    /// No backend is configured at all.
    NotConfigured,
    /// A server is configured but nothing is listening.
    Unreachable(String),
    /// A server answered but has no usable model.
    NoModel(String),
    /// The call started and failed.
    Failed(String),
    /// The user interrupted it.
    Interrupted,
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::NotConfigured => write!(f, "no local model is configured"),
            ModelError::Unreachable(s) => write!(f, "{s}"),
            ModelError::NoModel(s) => write!(f, "{s}"),
            ModelError::Failed(s) => write!(f, "{s}"),
            ModelError::Interrupted => write!(f, "interrupted"),
        }
    }
}

impl std::error::Error for ModelError {}

/// What a backend can tell us about itself without being asked a question.
#[derive(Debug, Clone)]
pub struct Availability {
    pub reachable: bool,
    pub endpoint: String,
    pub models: Vec<String>,
    /// The model a call would actually use.
    pub selected: Option<String>,
    pub detail: Option<String>,
}

pub trait Backend {
    fn name(&self) -> &'static str;

    /// Ask the server what it has. Never fails loudly: an unreachable server
    /// is reported as `reachable: false` with an explanation, because on this
    /// path "nothing is running" is the expected answer more often than not.
    fn availability(&self) -> Availability;

    /// Send a conversation and stream the reply.
    ///
    /// `on_token` is called with each fragment as it arrives so the caller can
    /// print it immediately. Returning `false` stops generation, which is how
    /// an interrupt does not leave the server working on an answer nobody will
    /// read.
    fn chat(
        &self,
        messages: &[Message],
        on_token: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, ModelError>;
}

/// How a streamed reply ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// The server said it was done, of its own accord.
    Complete,
    /// The server stopped because the reply reached the token limit.
    Limit,
    /// The stream closed without the server saying it was done.
    Cut,
}

/// Turn what a stream produced into the call's result.
///
/// A reply with no answer in it is a failure however the stream ended: an
/// empty answer reported as a success is the one outcome nobody can act on.
/// An answer that stopped short is kept, with a line saying so, because half
/// an answer is worth reading once you know it is half.
///
/// `thought` is whether the model sent reasoning before (or instead of) an
/// answer, which is the usual reason a limit is reached with nothing to show.
pub(crate) fn settle(
    mut full: String,
    ending: Ending,
    thought: bool,
    max_tokens: u32,
    on_token: &mut dyn FnMut(&str) -> bool,
) -> Result<String, ModelError> {
    if full.trim().is_empty() {
        return Err(ModelError::Failed(match (ending, thought) {
            (Ending::Limit, true) => format!(
                "the model spent its whole allowance of {max_tokens} tokens thinking and never \
                 started the answer. Raise max_tokens under [model] in {}, or use a model that \
                 does not think before answering.",
                crate::config::tilde(&Config::path())
            ),
            (Ending::Limit, false) => format!(
                "the model reached its limit of {max_tokens} tokens without writing an answer. \
                 Raise max_tokens under [model] in {}.",
                crate::config::tilde(&Config::path())
            ),
            (Ending::Complete, _) => "the model finished without writing an answer.".into(),
            (Ending::Cut, _) => {
                "the connection to the model server closed before any answer arrived.".into()
            }
        }));
    }
    let note = match ending {
        Ending::Complete => return Ok(full),
        Ending::Limit => format!(
            "\n\n[Cut off: the answer reached the limit of {max_tokens} tokens. Raise max_tokens \
             under [model] in {} for longer answers.]",
            crate::config::tilde(&Config::path())
        ),
        Ending::Cut => "\n\n[Cut off: the connection closed before the answer finished.]".into(),
    };
    // The note is the last thing sent, so a sink that wants to stop now has
    // nothing further to refuse.
    let _ = on_token(&note);
    full.push_str(&note);
    Ok(full)
}

/// Build the backend named in the config.
///
/// `none` is a real, supported choice rather than a broken state: it means
/// "run the rules, skip the prose", which is a reasonable way to use Oracle on
/// a machine with 4GB of RAM.
pub fn backend_for(cfg: &Config) -> Result<Box<dyn Backend>, String> {
    let name = cfg.model.backend.trim().to_ascii_lowercase();
    match name.as_str() {
        "none" | "" => Err("no model backend is configured".into()),
        "ollama" => {
            let url = cfg.endpoint()?;
            Ok(Box::new(ollama::Ollama::new(url, cfg)))
        }
        "llamacpp" | "llama.cpp" | "llama-server" | "openai" => {
            let url = cfg.endpoint()?;
            Ok(Box::new(llamacpp::LlamaCpp::new(url, cfg)))
        }
        other => Err(format!(
            "unknown backend {other:?}; known backends are ollama, llamacpp and none"
        )),
    }
}

/// What to tell someone who has no model and asked a question that needs one.
///
/// Deliberately short, and deliberately not a sales pitch. It states the
/// requirement once and leaves. Oracle does not install software, does not
/// offer to, and does not repeat itself on the next run.
pub fn no_model_advice(cfg: &Config) -> String {
    let backend = cfg.model.backend.trim();
    if backend.is_empty() || backend == "none" {
        return "Oracle has no model configured, so it can run checks but cannot answer in \
                prose. `oracle doctor` works without one. To add one, run `oracle setup`."
            .to_string();
    }
    format!(
        "Oracle could not reach the {backend} server at {}. `oracle doctor` still works \
         without a model. To point Oracle somewhere else, run `oracle setup`.",
        cfg.model.endpoint
    )
}

/// A tiny HTTP server for exercising the backends against a real socket.
///
/// The HTTP client in `http` is written here rather than taken from a crate,
/// so it has to be tested against something that actually speaks the protocol.
/// These helpers serve one canned response on an ephemeral port; the backends
/// then run unmodified against it, which covers request shaping, chunked
/// decoding, and streaming, none of which a unit test on a parser would reach.
#[cfg(test)]
pub mod testserver {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    pub struct Server {
        pub port: u16,
        bodies: Arc<Mutex<Vec<String>>>,
    }

    impl Server {
        /// Serve `responses` in order, one per connection, then stop.
        ///
        /// The listener is non-blocking and the thread gives up after a short
        /// idle period. A blocking `accept()` looks simpler and hangs the
        /// whole test binary whenever a test queues more responses than the
        /// client asks for -- which several of these do deliberately, to prove
        /// a backend stops early.
        pub fn start(responses: Vec<Vec<u8>>) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
            let port = listener.local_addr().unwrap().port();
            listener
                .set_nonblocking(true)
                .expect("a non-blocking listener");
            let bodies = Arc::new(Mutex::new(Vec::new()));
            let recorded = bodies.clone();

            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                for response in responses {
                    let stream = loop {
                        match listener.accept() {
                            Ok((s, _)) => break Some(s),
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                if std::time::Instant::now() >= deadline {
                                    break None;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(5));
                            }
                            Err(_) => break None,
                        }
                    };
                    let Some(mut stream) = stream else { return };
                    let _ = stream.set_nonblocking(false);

                    // Read the request head, and its body when one is declared,
                    // so the client never sees a broken pipe mid-write.
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut length = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break;
                        }
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            break;
                        }
                        if let Some(v) =
                            trimmed.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            length = v.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0u8; length];
                    if length > 0 {
                        let _ = reader.read_exact(&mut body);
                    }
                    // Recorded before replying, so a client that has its
                    // answer can rely on its request being here.
                    recorded
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&body).into_owned());
                    let _ = stream.write_all(&response);
                    let _ = stream.flush();
                }
            });

            Server { port, bodies }
        }

        /// The request bodies received so far, in order.
        pub fn bodies(&self) -> Vec<String> {
            self.bodies.lock().unwrap().clone()
        }

        pub fn url(&self) -> crate::http::Url {
            crate::http::Url::parse(&format!("http://127.0.0.1:{}", self.port)).unwrap()
        }
    }

    /// A complete response with a `Content-Length` body.
    pub fn sized(content_type: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// A chunked response whose body arrives as the given pieces.
    ///
    /// This is how both servers actually stream, and it is the part of the
    /// client most likely to be subtly wrong.
    pub fn chunked(content_type: &str, pieces: &[&str]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
             Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )
        .into_bytes();
        for p in pieces {
            out.extend_from_slice(format!("{:x}\r\n", p.len()).as_bytes());
            out.extend_from_slice(p.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    pub fn error(status: u16, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_a_choice_not_a_typo() {
        let mut cfg = Config::default();
        cfg.model.backend = "none".into();
        assert!(backend_for(&cfg).is_err());
        assert!(no_model_advice(&cfg).contains("doctor"));
    }

    #[test]
    fn an_unknown_backend_names_the_ones_that_exist() {
        let mut cfg = Config::default();
        cfg.model.backend = "gpt4all".into();
        let err = backend_for(&cfg).err().expect("must refuse");
        assert!(err.contains("ollama"), "got {err}");
        assert!(err.contains("llamacpp"), "got {err}");
    }

    #[test]
    fn a_remote_endpoint_is_refused_before_any_backend_is_built() {
        let mut cfg = Config::default();
        cfg.model.endpoint = "http://203.0.113.5:11434".into();
        let err = backend_for(&cfg).err().expect("must refuse");
        assert!(err.contains("allow_remote_endpoint"), "got {err}");
    }

    #[test]
    fn both_backend_names_build() {
        let mut cfg = Config::default();
        cfg.model.backend = "ollama".into();
        assert!(backend_for(&cfg).is_ok());
        cfg.model.backend = "llamacpp".into();
        cfg.model.endpoint = "http://127.0.0.1:8080".into();
        assert!(backend_for(&cfg).is_ok());
    }
}
