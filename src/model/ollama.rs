//! The Ollama backend.
//!
//! Ollama is the default because it is the one an ordinary person can already
//! have running, and because it manages weights itself -- Oracle never
//! downloads a model, which keeps it out of the business of writing several
//! gigabytes to somebody's disk.
//!
//! The API is `/api/chat` with `stream: true`, which returns newline-delimited
//! JSON objects rather than server-sent events.

use super::{Availability, Backend, Message, ModelError};
use crate::config::Config;
use crate::http::{self, Url};
use serde_json::{Value, json};
use std::time::Duration;

pub struct Ollama {
    url: Url,
    model: String,
    timeout: Duration,
    temperature: f32,
    max_tokens: u32,
}

impl Ollama {
    pub fn new(url: Url, cfg: &Config) -> Ollama {
        Ollama {
            url,
            model: cfg.model.name.clone(),
            timeout: cfg.timeout(),
            temperature: cfg.model.temperature,
            max_tokens: cfg.model.max_tokens,
        }
    }

    fn list_models(&self) -> Result<Vec<String>, String> {
        let url = self.url.join("/api/tags");
        let resp = http::get(&url, Duration::from_secs(5)).map_err(|e| e.to_string())?;
        let body = resp.text().map_err(|e| e.to_string())?;
        let parsed: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;

        let mut names: Vec<String> = parsed
            .get("models")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        Ok(names)
    }

    /// The model to use: the configured one when it is present, otherwise
    /// whatever the server has.
    ///
    /// Falling back rather than failing means Oracle keeps working after
    /// somebody removes the model named in their config, which is a thing
    /// people do without connecting it to this tool.
    fn choose(&self, available: &[String]) -> Option<String> {
        if !self.model.is_empty() {
            // Ollama compares names without regard to case, so `:35b` names
            // the model the server lists as `:35B`. The server's own spelling
            // is what gets sent.
            let wanted = self.model.trim();
            if let Some(exact) = available.iter().find(|m| m.eq_ignore_ascii_case(wanted)) {
                return Some(exact.clone());
            }
            // Ollama names carry a tag; `qwen2.5` should match `qwen2.5:7b`.
            if let Some(prefix) = available.iter().find(|m| {
                m.split(':')
                    .next()
                    .is_some_and(|base| base.eq_ignore_ascii_case(wanted))
            }) {
                return Some(prefix.clone());
            }
            // Configured but absent: say so rather than quietly using another.
            return None;
        }
        prefer_small_instruct(available)
    }
}

/// Pick a sensible default from what is installed.
///
/// Preference goes to instruction-tuned models, then to smaller ones, because
/// Oracle's job is short factual answers on a machine that may be a laptop
/// with integrated graphics and no GPU acceleration worth the name.
fn prefer_small_instruct(available: &[String]) -> Option<String> {
    if available.is_empty() {
        return None;
    }
    let score = |name: &str| -> i32 {
        let n = name.to_ascii_lowercase();
        let mut s = 0;
        if n.contains("instruct") || n.contains("chat") {
            s += 10;
        }
        // Rough size preference from the tag.
        for (needle, bonus) in [
            ("1b", 6),
            ("1.5b", 6),
            ("3b", 8),
            ("4b", 7),
            ("7b", 5),
            ("8b", 4),
            ("13b", 1),
            ("14b", 1),
        ] {
            if n.contains(needle) {
                s += bonus;
                break;
            }
        }
        // Embedding models cannot chat at all.
        if n.contains("embed") || n.contains("bge") || n.contains("minilm") {
            s -= 100;
        }
        s
    };

    available
        .iter()
        .max_by(|a, b| score(a).cmp(&score(b)).then(b.len().cmp(&a.len())))
        .cloned()
}

impl Backend for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn availability(&self) -> Availability {
        match self.list_models() {
            Ok(models) => {
                let selected = self.choose(&models);
                let detail = if models.is_empty() {
                    Some(
                        "The server is running but has no models. `ollama pull qwen2.5:3b-instruct` \
                         gives it one."
                            .into(),
                    )
                } else if selected.is_none() {
                    Some(format!(
                        "The configured model {:?} is not installed on this server.",
                        self.model
                    ))
                } else {
                    None
                };
                Availability {
                    reachable: true,
                    endpoint: self.url.origin(),
                    models,
                    selected,
                    detail,
                }
            }
            Err(e) => Availability {
                reachable: false,
                endpoint: self.url.origin(),
                models: Vec::new(),
                selected: None,
                detail: Some(e),
            },
        }
    }

    fn chat(
        &self,
        messages: &[Message],
        on_token: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, ModelError> {
        // The inner error already names the endpoint and says what went
        // wrong; wrapping it again produced the address twice in one sentence.
        let available = self.list_models().map_err(ModelError::Unreachable)?;

        let model = self.choose(&available).ok_or_else(|| {
            if available.is_empty() {
                ModelError::NoModel(format!(
                    "{} is running but has no models installed",
                    self.url.origin()
                ))
            } else {
                ModelError::NoModel(format!(
                    "the configured model {:?} is not installed; this server has: {}",
                    self.model,
                    available.join(", ")
                ))
            }
        })?;

        let body = json!({
            "model": model,
            "stream": true,
            "messages": messages.iter().map(|m| json!({
                "role": m.role.as_str(),
                "content": m.content,
            })).collect::<Vec<_>>(),
            "options": {
                "temperature": self.temperature,
                "num_predict": self.max_tokens,
            },
        });

        let url = self.url.join("/api/chat");
        let resp = http::post_json(&url, body.to_string().as_bytes(), self.timeout)
            .map_err(|e| ModelError::Failed(e.to_string()))?;

        let mut full = String::new();
        let mut stopped = false;

        resp.for_each_line(|line| {
            let line = line.trim();
            if line.is_empty() {
                return true;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                // A line that is not JSON is not worth aborting a good answer
                // over; skip it and keep reading.
                return true;
            };
            if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                full.push_str(&format!("\n[server error: {err}]"));
                return false;
            }
            if let Some(chunk) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                && !chunk.is_empty()
            {
                full.push_str(chunk);
                if !on_token(chunk) {
                    stopped = true;
                    return false;
                }
            }
            !v.get("done").and_then(|d| d.as_bool()).unwrap_or(false)
        })
        .map_err(|e| ModelError::Failed(e.to_string()))?;

        if stopped {
            return Err(ModelError::Interrupted);
        }
        Ok(full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ollama(configured: &str) -> Ollama {
        let mut cfg = Config::default();
        cfg.model.name = configured.into();
        Ollama::new(Url::parse("http://127.0.0.1:11434").unwrap(), &cfg)
    }

    #[test]
    fn a_configured_model_is_matched_without_its_tag() {
        let o = ollama("qwen2.5");
        let have = vec!["llama3.2:3b".to_string(), "qwen2.5:7b-instruct".to_string()];
        assert_eq!(o.choose(&have).as_deref(), Some("qwen2.5:7b-instruct"));
    }

    #[test]
    fn an_exact_name_wins() {
        let o = ollama("llama3.2:3b");
        let have = vec!["llama3.2:3b".to_string(), "llama3.2:1b".to_string()];
        assert_eq!(o.choose(&have).as_deref(), Some("llama3.2:3b"));
    }

    #[test]
    fn a_name_is_matched_regardless_of_case_and_sent_as_the_server_spells_it() {
        let o = ollama("ornith-1.5:35b");
        let have = vec!["ornith-1.5:35B".to_string(), "ornith-1.5:9b".to_string()];
        assert_eq!(o.choose(&have).as_deref(), Some("ornith-1.5:35B"));
        assert_eq!(
            ollama("ORNITH-1.5").choose(&have).as_deref(),
            Some("ornith-1.5:35B")
        );
    }

    #[test]
    fn a_tag_that_does_not_exist_is_still_absent_whatever_its_case() {
        let have = vec!["ornith-1.5:35B".to_string(), "ornith-1.5:9b".to_string()];
        assert_eq!(ollama("ornith-1.5:latest").choose(&have), None);
    }

    #[test]
    fn a_configured_model_that_is_absent_is_reported_rather_than_swapped() {
        let o = ollama("mistral");
        let have = vec!["llama3.2:3b".to_string()];
        assert_eq!(
            o.choose(&have),
            None,
            "silently answering with a different model is worse than saying the model is gone"
        );
    }

    #[test]
    fn with_nothing_configured_a_small_instruct_model_is_preferred() {
        let o = ollama("");
        let have = vec![
            "llama3.1:70b".to_string(),
            "qwen2.5:3b-instruct".to_string(),
            "codellama:13b".to_string(),
        ];
        assert_eq!(o.choose(&have).as_deref(), Some("qwen2.5:3b-instruct"));
    }

    #[test]
    fn embedding_models_are_never_chosen_to_chat_with() {
        let o = ollama("");
        let have = vec!["nomic-embed-text".to_string(), "llama3.2:3b".to_string()];
        assert_eq!(o.choose(&have).as_deref(), Some("llama3.2:3b"));
    }

    #[test]
    fn no_models_means_no_choice() {
        assert_eq!(ollama("").choose(&[]), None);
    }

    #[test]
    fn an_unreachable_server_is_reported_not_panicked_over() {
        let mut cfg = Config::default();
        // Port 1 has nothing on it, on any machine.
        cfg.model.endpoint = "http://127.0.0.1:1".into();
        let o = Ollama::new(Url::parse(&cfg.model.endpoint).unwrap(), &cfg);
        let a = o.availability();
        assert!(!a.reachable);
        assert!(a.detail.is_some());
    }
}

/// End-to-end tests against a socket serving real Ollama-shaped responses.
#[cfg(test)]
mod wire {
    use super::*;
    use crate::model::testserver::{Server, chunked, error, sized};

    fn config_for(server: &Server, model: &str) -> (Config, Ollama) {
        let mut cfg = Config::default();
        cfg.model.name = model.into();
        cfg.model.endpoint = format!("http://127.0.0.1:{}", server.port);
        let o = Ollama::new(server.url(), &cfg);
        (cfg, o)
    }

    const TAGS: &str = r#"{"models":[{"name":"qwen2.5:3b-instruct"},{"name":"nomic-embed-text"}]}"#;

    fn ndjson(pieces: &[&str]) -> Vec<u8> {
        chunked("application/x-ndjson", pieces)
    }

    #[test]
    fn availability_lists_models_and_picks_one_that_can_chat() {
        let server = Server::start(vec![sized("application/json", TAGS)]);
        let (_cfg, o) = config_for(&server, "");
        let a = o.availability();
        assert!(a.reachable);
        assert_eq!(a.models.len(), 2);
        assert_eq!(a.selected.as_deref(), Some("qwen2.5:3b-instruct"));
    }

    #[test]
    fn a_streamed_answer_arrives_token_by_token_and_in_order() {
        let server = Server::start(vec![
            sized("application/json", TAGS),
            ndjson(&[
                "{\"message\":{\"content\":\"The disk \"},\"done\":false}\n",
                "{\"message\":{\"content\":\"is full\"},\"done\":false}\n",
                "{\"message\":{\"content\":\".\"},\"done\":false}\n",
                "{\"message\":{\"content\":\"\"},\"done\":true}\n",
            ]),
        ]);
        let (_cfg, o) = config_for(&server, "");

        let mut seen: Vec<String> = Vec::new();
        let full = o
            .chat(&[Message::user("why")], &mut |t| {
                seen.push(t.to_string());
                true
            })
            .expect("the stream should complete");

        assert_eq!(full, "The disk is full.");
        assert_eq!(seen, vec!["The disk ", "is full", "."]);
    }

    #[test]
    fn a_json_object_split_across_two_chunks_is_still_read_whole() {
        // Chunk boundaries have nothing to do with line boundaries; a decoder
        // that assumes they line up loses tokens at random.
        let server = Server::start(vec![
            sized("application/json", TAGS),
            ndjson(&[
                "{\"message\":{\"content\":\"half \"",
                "},\"done\":false}\n{\"message\":{\"content\":\"and half\"},\"done\":true}\n",
            ]),
        ]);
        let (_cfg, o) = config_for(&server, "");
        let full = o.chat(&[Message::user("q")], &mut |_| true).unwrap();
        assert_eq!(full, "half and half");
    }

    #[test]
    fn returning_false_from_the_sink_stops_generation() {
        let server = Server::start(vec![
            sized("application/json", TAGS),
            ndjson(&[
                "{\"message\":{\"content\":\"one \"},\"done\":false}\n",
                "{\"message\":{\"content\":\"two \"},\"done\":false}\n",
                "{\"message\":{\"content\":\"three\"},\"done\":true}\n",
            ]),
        ]);
        let (_cfg, o) = config_for(&server, "");

        let mut count = 0;
        let err = o
            .chat(&[Message::user("q")], &mut |_| {
                count += 1;
                false
            })
            .expect_err("stopping must be reported, not silently truncated");

        assert!(matches!(err, ModelError::Interrupted));
        assert_eq!(count, 1, "generation should stop at the first refusal");
    }

    #[test]
    fn a_server_with_no_models_says_so_rather_than_failing_obscurely() {
        let server = Server::start(vec![
            sized("application/json", r#"{"models":[]}"#),
            sized("application/json", r#"{"models":[]}"#),
        ]);
        let (_cfg, o) = config_for(&server, "");
        assert!(o.availability().detail.unwrap().contains("no models"));
        let err = o.chat(&[Message::user("q")], &mut |_| true).unwrap_err();
        assert!(matches!(err, ModelError::NoModel(_)));
    }

    #[test]
    fn a_configured_model_the_server_lacks_is_named_in_the_error() {
        // One response, because chat gives up after listing the models and
        // never opens a second connection.
        let server = Server::start(vec![sized("application/json", TAGS)]);
        let (_cfg, o) = config_for(&server, "mistral");
        let err = o.chat(&[Message::user("q")], &mut |_| true).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("mistral"), "got {msg}");
        assert!(msg.contains("qwen2.5:3b-instruct"), "got {msg}");
    }

    #[test]
    fn an_error_status_is_surfaced_with_the_servers_own_words() {
        let server = Server::start(vec![error(500, "model runner has crashed")]);
        let (_cfg, o) = config_for(&server, "");
        let a = o.availability();
        assert!(!a.reachable);
        assert!(
            a.detail.unwrap().contains("model runner has crashed"),
            "the server usually explains itself; pass that through"
        );
    }

    #[test]
    fn a_malformed_line_mid_stream_does_not_abort_a_good_answer() {
        let server = Server::start(vec![
            sized("application/json", TAGS),
            ndjson(&[
                "{\"message\":{\"content\":\"good \"},\"done\":false}\n",
                "this line is not json\n",
                "{\"message\":{\"content\":\"answer\"},\"done\":true}\n",
            ]),
        ]);
        let (_cfg, o) = config_for(&server, "");
        let full = o.chat(&[Message::user("q")], &mut |_| true).unwrap();
        assert_eq!(full, "good answer");
    }
}
