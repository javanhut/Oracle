//! The llama.cpp backend.
//!
//! `llama-server` speaks the OpenAI chat-completions shape, so this backend
//! also covers anything else that does -- llamafile, LM Studio's local server,
//! vLLM -- as long as it is on this machine. That breadth is why it exists:
//! someone who already runs a model server should not have to install a second
//! one to try Oracle.
//!
//! The stream is server-sent events: `data: {json}` per line, ending with
//! `data: [DONE]`.

use super::{Availability, Backend, Ending, Message, ModelError, settle};
use crate::config::Config;
use crate::http::{self, Url};
use serde_json::{Value, json};
use std::time::Duration;

pub struct LlamaCpp {
    url: Url,
    model: String,
    timeout: Duration,
    temperature: f32,
    max_tokens: u32,
}

impl LlamaCpp {
    pub fn new(url: Url, cfg: &Config) -> LlamaCpp {
        LlamaCpp {
            url,
            model: cfg.model.name.clone(),
            timeout: cfg.timeout(),
            temperature: cfg.model.temperature,
            max_tokens: cfg.model.max_tokens,
        }
    }

    fn list_models(&self) -> Result<Vec<String>, String> {
        let url = self.url.join("/v1/models");
        let resp = http::get(&url, Duration::from_secs(5)).map_err(|e| e.to_string())?;
        let body = resp.text().map_err(|e| e.to_string())?;
        let parsed: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        Ok(parsed
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl Backend for LlamaCpp {
    fn name(&self) -> &'static str {
        "llamacpp"
    }

    fn availability(&self) -> Availability {
        match self.list_models() {
            Ok(models) => {
                // llama-server loads exactly one model and will answer to any
                // name, so an empty list is not a failure the way it is for
                // Ollama -- it just means this build does not list models.
                let selected = if !self.model.is_empty() {
                    Some(self.model.clone())
                } else {
                    models.first().cloned().or(Some("(server default)".into()))
                };
                Availability {
                    reachable: true,
                    endpoint: self.url.origin(),
                    models,
                    selected,
                    detail: None,
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
        let model = if self.model.is_empty() {
            // llama-server ignores this field; something has to be sent.
            "local".to_string()
        } else {
            self.model.clone()
        };

        let body = json!({
            "model": model,
            "stream": true,
            "temperature": self.temperature,
            "max_tokens": self.max_tokens,
            "messages": messages.iter().map(|m| json!({
                "role": m.role.as_str(),
                "content": m.content,
            })).collect::<Vec<_>>(),
        });

        let url = self.url.join("/v1/chat/completions");
        let resp = http::post_json(&url, body.to_string().as_bytes(), self.timeout).map_err(
            |e| match e {
                http::HttpError::Connect(s) => {
                    ModelError::Unreachable(format!("{} is not answering: {s}", self.url.origin()))
                }
                other => ModelError::Failed(other.to_string()),
            },
        )?;

        let mut full = String::new();
        let mut stopped = false;
        let mut thought = false;
        let mut ending = Ending::Cut;
        let mut server_error = None;

        resp.for_each_line(|line| {
            let line = line.trim();
            let Some(payload) = line.strip_prefix("data:") else {
                return true;
            };
            let payload = payload.trim();
            if payload == "[DONE]" {
                if ending == Ending::Cut {
                    ending = Ending::Complete;
                }
                return false;
            }
            let Ok(v) = serde_json::from_str::<Value>(payload) else {
                return true;
            };
            if let Some(err) = v.get("error") {
                let msg = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("no reason given");
                server_error = Some(msg.to_string());
                return false;
            }
            let choice = v.get("choices").and_then(|c| c.get(0));
            let delta = choice.and_then(|c| c.get("delta"));
            // Reasoning models on llama-server send their thinking here.
            if delta
                .and_then(|d| d.get("reasoning_content"))
                .and_then(|r| r.as_str())
                .is_some_and(|r| !r.is_empty())
            {
                thought = true;
            }
            if let Some(chunk) = delta
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
                && !chunk.is_empty()
            {
                full.push_str(chunk);
                if !on_token(chunk) {
                    stopped = true;
                    return false;
                }
            }
            match choice
                .and_then(|c| c.get("finish_reason"))
                .and_then(|r| r.as_str())
            {
                Some("length") => ending = Ending::Limit,
                Some(_) => ending = Ending::Complete,
                None => {}
            }
            true
        })
        .map_err(|e| ModelError::Failed(e.to_string()))?;

        if stopped {
            return Err(ModelError::Interrupted);
        }
        if let Some(err) = server_error {
            return Err(ModelError::Failed(format!(
                "the model server reported an error: {err}"
            )));
        }
        settle(full, ending, thought, self.max_tokens, on_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreachable_server_reports_rather_than_fails_hard() {
        let mut cfg = Config::default();
        cfg.model.endpoint = "http://127.0.0.1:1".into();
        let l = LlamaCpp::new(Url::parse(&cfg.model.endpoint).unwrap(), &cfg);
        let a = l.availability();
        assert!(!a.reachable);
        assert_eq!(l.name(), "llamacpp");
    }

    #[test]
    fn the_chat_path_is_the_openai_one() {
        let cfg = Config::default();
        let l = LlamaCpp::new(Url::parse("http://127.0.0.1:8080").unwrap(), &cfg);
        assert_eq!(
            l.url.join("/v1/chat/completions").path,
            "/v1/chat/completions"
        );
    }

    #[test]
    fn a_base_path_is_preserved_when_joining() {
        let cfg = Config::default();
        let l = LlamaCpp::new(Url::parse("http://127.0.0.1:8080/llm").unwrap(), &cfg);
        assert_eq!(l.url.join("/v1/models").path, "/llm/v1/models");
    }
}

/// End-to-end tests against a socket serving real server-sent-event responses.
#[cfg(test)]
mod wire {
    use super::*;
    use crate::model::testserver::{Server, chunked, error, sized};

    fn backend(server: &Server, model: &str) -> LlamaCpp {
        let mut cfg = Config::default();
        cfg.model.name = model.into();
        cfg.model.endpoint = format!("http://127.0.0.1:{}", server.port);
        LlamaCpp::new(server.url(), &cfg)
    }

    fn sse(pieces: &[&str]) -> Vec<u8> {
        chunked("text/event-stream", pieces)
    }

    fn delta(content: &str) -> String {
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{content}\"}}}}]}}\n\n")
    }

    #[test]
    fn a_streamed_answer_is_assembled_in_order() {
        let server = Server::start(vec![sse(&[
            &delta("Swap "),
            &delta("is full"),
            &delta("."),
            "data: [DONE]\n\n",
        ])]);
        let l = backend(&server, "");

        let mut seen = Vec::new();
        let full = l
            .chat(&[Message::user("why slow")], &mut |t| {
                seen.push(t.to_string());
                true
            })
            .expect("the stream should complete");

        assert_eq!(full, "Swap is full.");
        assert_eq!(seen, vec!["Swap ", "is full", "."]);
    }

    #[test]
    fn the_done_sentinel_ends_the_stream_without_becoming_text() {
        let server = Server::start(vec![sse(&[&delta("answer"), "data: [DONE]\n\n"])]);
        let l = backend(&server, "");
        let full = l.chat(&[Message::user("q")], &mut |_| true).unwrap();
        assert_eq!(full, "answer", "the sentinel must not leak into the answer");
    }

    #[test]
    fn an_event_split_across_chunks_survives() {
        let server = Server::start(vec![sse(&[
            "data: {\"choices\":[{\"delta\":{\"content\":\"split ",
            "answer\"}}]}\n\ndata: [DONE]\n\n",
        ])]);
        let l = backend(&server, "");
        assert_eq!(
            l.chat(&[Message::user("q")], &mut |_| true).unwrap(),
            "split answer"
        );
    }

    #[test]
    fn a_keepalive_comment_between_events_is_ignored() {
        // Servers send `: ping` lines to hold the connection open.
        let server = Server::start(vec![sse(&[
            &delta("a"),
            ": ping\n\n",
            &delta("b"),
            "data: [DONE]\n\n",
        ])]);
        let l = backend(&server, "");
        assert_eq!(l.chat(&[Message::user("q")], &mut |_| true).unwrap(), "ab");
    }

    #[test]
    fn returning_false_from_the_sink_stops_generation() {
        let server = Server::start(vec![sse(&[
            &delta("one"),
            &delta("two"),
            "data: [DONE]\n\n",
        ])]);
        let l = backend(&server, "");

        let mut count = 0;
        let err = l
            .chat(&[Message::user("q")], &mut |_| {
                count += 1;
                false
            })
            .expect_err("stopping must be reported");
        assert!(matches!(err, ModelError::Interrupted));
        assert_eq!(count, 1);
    }

    #[test]
    fn availability_reads_the_model_list_when_the_server_offers_one() {
        let server = Server::start(vec![sized(
            "application/json",
            r#"{"data":[{"id":"qwen2.5-3b-instruct.gguf"}]}"#,
        )]);
        let a = backend(&server, "").availability();
        assert!(a.reachable);
        assert_eq!(a.models, vec!["qwen2.5-3b-instruct.gguf"]);
        assert_eq!(a.selected.as_deref(), Some("qwen2.5-3b-instruct.gguf"));
    }

    #[test]
    fn a_server_that_lists_nothing_is_still_usable() {
        // llama-server loads exactly one model and answers to any name, so an
        // empty list is not the failure it would be for Ollama.
        let server = Server::start(vec![sized("application/json", r#"{"data":[]}"#)]);
        let a = backend(&server, "").availability();
        assert!(a.reachable);
        assert_eq!(a.selected.as_deref(), Some("(server default)"));
    }

    #[test]
    fn an_error_payload_mid_stream_is_surfaced_rather_than_swallowed() {
        let server = Server::start(vec![sse(&[
            "data: {\"error\":{\"message\":\"context window exceeded\"}}\n\n",
        ])]);
        let l = backend(&server, "");
        let err = l
            .chat(&[Message::user("q")], &mut |_| true)
            .expect_err("a server error must not look like a finished answer");
        assert!(matches!(err, ModelError::Failed(_)));
        assert!(
            err.to_string().contains("context window exceeded"),
            "got {err}"
        );
    }

    #[test]
    fn reasoning_until_the_limit_fails_and_says_why() {
        let server = Server::start(vec![sse(&[
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Let me think\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
            "data: [DONE]\n\n",
        ])]);
        let l = backend(&server, "");
        let err = l.chat(&[Message::user("q")], &mut |_| true).unwrap_err();
        assert!(err.to_string().contains("thinking"), "got {err}");
    }

    #[test]
    fn an_answer_cut_off_by_the_limit_is_kept_and_marked() {
        let server = Server::start(vec![sse(&[
            &delta("Swap is"),
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
            "data: [DONE]\n\n",
        ])]);
        let l = backend(&server, "");
        let full = l.chat(&[Message::user("q")], &mut |_| true).unwrap();
        assert!(
            full.starts_with("Swap is") && full.contains("Cut off"),
            "got {full}"
        );
    }

    #[test]
    fn a_stream_with_no_answer_is_a_failure() {
        let server = Server::start(vec![sse(&["data: [DONE]\n\n"])]);
        let l = backend(&server, "");
        assert!(l.chat(&[Message::user("q")], &mut |_| true).is_err());
    }

    #[test]
    fn an_http_error_is_reported_with_the_servers_words() {
        let server = Server::start(vec![error(400, "no model is loaded")]);
        let l = backend(&server, "");
        let err = l.chat(&[Message::user("q")], &mut |_| true).unwrap_err();
        assert!(err.to_string().contains("no model is loaded"), "got {err}");
    }
}
