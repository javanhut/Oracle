//! Configuration.
//!
//! Oracle has no config file until you run `oracle setup`. Everything below
//! has a default that works, so a missing file is not an error and never
//! produces a prompt to create one -- a tool that nags about its own
//! configuration is exactly the kind of intrusion this program is trying not
//! to be.
//!
//! The file lives at `~/.config/raven/oracle.toml`, beside the other Raven
//! per-user settings, but nothing else in the system reads it and nothing else
//! writes it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub model: ModelConfig,
    pub context: ContextConfig,
    pub privacy: PrivacyConfig,
    pub voice: VoiceConfig,
    pub behaviour: BehaviourConfig,
}

/// Which local inference server to talk to, and as what.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    /// `ollama`, `llamacpp`, or `none`.
    pub backend: String,
    pub endpoint: String,
    /// Empty means "whichever model the server offers first", which keeps a
    /// fresh install working before anyone has chosen.
    pub name: String,
    pub timeout_seconds: u64,
    /// 0.0-1.0. Low by default: this is diagnosis, not brainstorming.
    pub temperature: f32,
    /// Cap on the reply, in tokens. Long answers are rarely better answers.
    pub max_tokens: u32,
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig {
            backend: "ollama".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            name: String::new(),
            timeout_seconds: 180,
            temperature: 0.2,
            max_tokens: 900,
        }
    }
}

/// What Oracle is allowed to read about this machine.
///
/// Each switch is a category of probe. Turning one off does not merely hide it
/// from the report -- the probe does not run and its data never reaches the
/// model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextConfig {
    pub services: bool,
    pub storage: bool,
    pub network: bool,
    pub packages: bool,
    pub logs: bool,
    pub hardware: bool,
    /// How many recent log lines a probe may include.
    pub log_lines: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        ContextConfig {
            services: true,
            storage: true,
            network: true,
            packages: true,
            logs: true,
            hardware: true,
            log_lines: 40,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrivacyConfig {
    /// Scrub identifiers out of system context before it reaches the model.
    pub redact: bool,
    /// Permit a model endpoint that is not on this machine. Off by default,
    /// and `oracle` refuses to send anything to a non-loopback address until
    /// it is deliberately turned on.
    pub allow_remote_endpoint: bool,
    /// Keep a local record of questions and answers under the state directory.
    /// Off by default: Oracle should not accumulate a history of your problems
    /// unless you asked it to.
    pub keep_history: bool,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        PrivacyConfig {
            redact: true,
            allow_remote_endpoint: false,
            keep_history: false,
        }
    }
}

/// Dictation. Entirely optional, off until configured, and never listening
/// unless a command explicitly asks it to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VoiceConfig {
    pub enabled: bool,
    /// Path to a whisper.cpp binary (`whisper-cli`, or the older `main`).
    /// Empty means "look for one on PATH at the moment of use".
    pub whisper_bin: String,
    /// Path to a ggml model file.
    pub model: String,
    /// `auto`, `pw-record`, `arecord`, or `parecord`.
    pub recorder: String,
    /// Ceiling on a single recording, so a forgotten session cannot record the
    /// room indefinitely.
    pub max_seconds: u32,
    /// Whisper's language hint; `auto` detects.
    pub language: String,
    /// An escape hatch for a transcriber that is not whisper.cpp. `{audio}` is
    /// replaced with the WAV path; the command must print the transcript to
    /// stdout. When set, this wins over `whisper_bin`.
    pub command: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        VoiceConfig {
            enabled: false,
            whisper_bin: String::new(),
            model: String::new(),
            recorder: "auto".into(),
            max_seconds: 60,
            language: "en".into(),
            command: String::new(),
        }
    }
}

/// How far Oracle is allowed to go.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BehaviourConfig {
    /// When true -- the default, and the recommended setting -- Oracle prints
    /// commands and never runs them. See `--fix` in the README for what
    /// turning this off actually buys, which is less than it sounds.
    pub suggest_only: bool,
    /// Colour and formatting off, for piping into other tools.
    pub plain: bool,
}

impl Default for BehaviourConfig {
    fn default() -> Self {
        BehaviourConfig {
            suggest_only: true,
            plain: false,
        }
    }
}

impl Config {
    /// Load the config, or the defaults if there is no file.
    ///
    /// A file that exists but does not parse is a hard error. Silently falling
    /// back would ignore a preference someone wrote down and believed -- the
    /// same reasoning `ravend` applies to `login.toml`.
    pub fn load() -> Result<Config, String> {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).map_err(|e| format!("{} is not valid: {e}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    pub fn save(&self) -> Result<PathBuf, String> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let body = toml::to_string_pretty(self).map_err(|e| format!("cannot serialise: {e}"))?;
        let text = format!("{}{body}", Self::header());
        // Write-and-rename, so an interrupted save cannot leave a truncated
        // config that would then fail to parse on the next run.
        let tmp = path.with_extension("toml.new");
        std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
        Ok(path)
    }

    fn header() -> &'static str {
        "# Oracle -- an opt-in local troubleshooting companion.\n\
         #\n\
         # Written by `oracle setup`. Delete this file and Oracle goes back to\n\
         # its defaults; uninstall the binary and nothing here is read again.\n\
         # No other program on the system reads or writes this file.\n\n"
    }

    pub fn path() -> PathBuf {
        config_dir().join("oracle.toml")
    }

    pub fn exists() -> bool {
        Self::path().exists()
    }

    /// Resolve the model endpoint, refusing a remote one unless permitted.
    pub fn endpoint(&self) -> Result<crate::http::Url, String> {
        let url = crate::http::Url::parse(&self.model.endpoint).map_err(|e| e.to_string())?;
        if !url.is_loopback() && !self.privacy.allow_remote_endpoint {
            return Err(format!(
                "the configured endpoint {} is not on this machine.\n\
                 Oracle keeps your system details local unless you say otherwise.\n\
                 To allow it anyway, set allow_remote_endpoint = true under [privacy] in {}",
                url.origin(),
                Self::path().display()
            ));
        }
        Ok(url)
    }

    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.model.timeout_seconds.clamp(5, 3600))
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v).join("raven"),
        _ => home().join(".config").join("raven"),
    }
}

/// Where Oracle keeps anything it downloads or remembers: models, and the
/// history file if it was ever asked to keep one.
pub fn state_dir() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v).join("raven").join("oracle"),
        _ => home()
            .join(".local")
            .join("share")
            .join("raven")
            .join("oracle"),
    }
}

/// Shorten a path under `$HOME` to `~/...` for display.
pub fn tilde(p: &Path) -> String {
    let h = home();
    match p.strip_prefix(&h) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let c = Config::default();
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.model.backend, c.model.backend);
        assert!(back.privacy.redact);
        assert!(!back.voice.enabled);
        assert!(back.behaviour.suggest_only);
    }

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let c: Config = toml::from_str("[voice]\nenabled = true\n").unwrap();
        assert!(c.voice.enabled);
        assert_eq!(c.model.backend, "ollama");
        assert!(c.privacy.redact);
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_silently_ignored() {
        let r: Result<Config, _> = toml::from_str("[privacy]\nredakt = false\n");
        assert!(
            r.is_err(),
            "a typo in a privacy switch must not be swallowed"
        );
    }

    #[test]
    fn a_remote_endpoint_is_refused_by_default() {
        let mut c = Config::default();
        c.model.endpoint = "http://192.0.2.10:11434".into();
        assert!(c.endpoint().is_err());
        c.privacy.allow_remote_endpoint = true;
        assert!(c.endpoint().is_ok());
    }
}
