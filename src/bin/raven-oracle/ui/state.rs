//! What the window knows about the machine, with no GTK in it.
//!
//! The same split the terminal interface makes: the facts and the small
//! decisions about them live here as plain types, so they can be tested
//! without a display, and the pages only draw them.

use oracle::config::Config;
use oracle::model::Availability;
use oracle::probe::{Area, Finding, Severity, Suggestion};

/// Which findings the Findings page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    WarningsAndWorse,
    CriticalOnly,
}

impl Filter {
    pub const ALL: [Filter; 3] = [Filter::All, Filter::WarningsAndWorse, Filter::CriticalOnly];

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "Everything",
            Filter::WarningsAndWorse => "Warnings and worse",
            Filter::CriticalOnly => "Critical only",
        }
    }

    pub fn admits(self, s: Severity) -> bool {
        match self {
            Filter::All => true,
            Filter::WarningsAndWorse => s <= Severity::Warning,
            Filter::CriticalOnly => s == Severity::Critical,
        }
    }
}

/// The handful of facts about the machine the Overview shows.
#[derive(Debug, Clone, Default)]
pub struct Host {
    pub distro: String,
    pub kernel: String,
    pub init: String,
    pub uptime: String,
    pub desktop: Option<String>,
    pub root: bool,
}

/// What is known about the local model.
#[derive(Debug, Clone, Default)]
pub enum ModelState {
    #[default]
    Checking,
    Checked {
        backend: &'static str,
        availability: Availability,
    },
    /// No backend could be built: `none`, an unknown name, or a remote
    /// endpoint that has not been permitted.
    Off(String),
}

impl ModelState {
    /// Whether a question would get an answer: a server is up and has a model.
    pub fn ready(&self) -> bool {
        matches!(
            self,
            ModelState::Checked { availability, .. }
                if availability.reachable && availability.selected.is_some()
        )
    }

    /// One sentence for the person, whatever the state.
    pub fn describe(&self) -> String {
        match self {
            ModelState::Checking => "Looking for a local model…".into(),
            ModelState::Checked {
                backend,
                availability: a,
            } => {
                if !a.reachable {
                    a.detail
                        .clone()
                        .unwrap_or_else(|| format!("Nothing is answering at {}.", a.endpoint))
                } else if let Some(m) = &a.selected {
                    format!("{m} on {backend} at {}", a.endpoint)
                } else if a.models.is_empty() {
                    format!(
                        "{backend} is running at {} but has no models installed.",
                        a.endpoint
                    )
                } else {
                    // Up, with models, but not the one that was named. Say
                    // which it has: the usual cause is a tag that does not
                    // exist, like `:latest` for a model pulled as `:9b`.
                    const SHOWN: usize = 8;
                    let mut names = a.models.iter().take(SHOWN).cloned().collect::<Vec<_>>();
                    if a.models.len() > SHOWN {
                        names.push(format!("and {} more", a.models.len() - SHOWN));
                    }
                    let why = a.detail.clone().unwrap_or_else(|| {
                        "The model named in Settings is not on this server.".into()
                    });
                    format!("{why} It has: {}.", names.join(", "))
                }
            }
            ModelState::Off(e) => e.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub struct State {
    pub config: Config,
    pub scanning: bool,
    /// False until the first check completes, so no page can mistake "not
    /// looked yet" for a clean machine.
    pub scanned_once: bool,
    /// Bumped by every completed check, so pages rebuild only when the
    /// findings actually changed.
    pub revision: u64,
    pub findings: Vec<Finding>,
    pub host: Host,
    /// Exactly what a model would be sent, as of the last check.
    pub context: String,
    pub checked_at: String,
    pub model: ModelState,
}

impl State {
    pub fn visible(&self, filter: Filter) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| filter.admits(f.severity))
            .collect()
    }

    pub fn count(&self, s: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == s).count()
    }

    pub fn worst(&self) -> Option<Severity> {
        self.findings.iter().map(|f| f.severity).min()
    }
}

pub fn area_enabled(cfg: &Config, area: Area) -> bool {
    let c = &cfg.context;
    match area {
        Area::Services => c.services,
        Area::Storage => c.storage,
        Area::Network => c.network,
        Area::Packages => c.packages,
        Area::Logs => c.logs,
        Area::Hardware => c.hardware,
    }
}

pub fn set_area(cfg: &mut Config, area: Area, on: bool) {
    let c = &mut cfg.context;
    match area {
        Area::Services => c.services = on,
        Area::Storage => c.storage = on,
        Area::Network => c.network = on,
        Area::Packages => c.packages = on,
        Area::Logs => c.logs = on,
        Area::Hardware => c.hardware = on,
    }
}

/// The areas Settings says not to look at, so an empty report is never
/// mistaken for a clean bill of health it did not earn.
pub fn skipped_areas(cfg: &Config) -> Vec<&'static str> {
    Area::all()
        .into_iter()
        .filter(|a| !area_enabled(cfg, *a))
        .map(|a| a.name())
        .collect()
}

pub fn area_title(area: Area) -> &'static str {
    match area {
        Area::Services => "Services",
        Area::Storage => "Storage",
        Area::Network => "Network",
        Area::Packages => "Packages",
        Area::Logs => "Logs",
        Area::Hardware => "Hardware",
    }
}

pub fn area_description(area: Area) -> &'static str {
    match area {
        Area::Services => "Enabled services and whether the programs they start exist",
        Area::Storage => "Free space, inodes and mounts",
        Area::Network => "Interfaces, routes and DNS, read without sending anything",
        Area::Packages => "The rvn and pacman databases",
        Area::Logs => "Recent errors in the system logs",
        Area::Hardware => "Memory, temperature, battery and firmware",
    }
}

/// The backends Settings offers, as (config name, label).
pub const BACKENDS: [(&str, &str); 3] = [
    ("ollama", "Ollama"),
    ("llamacpp", "llama.cpp"),
    ("none", "None: checks only"),
];

/// Where a config's backend sits in [`BACKENDS`], accepting every spelling
/// the command line accepts.
pub fn backend_index(name: &str) -> u32 {
    match name.trim().to_ascii_lowercase().as_str() {
        "llamacpp" | "llama.cpp" | "llama-server" | "openai" => 1,
        "none" | "" => 2,
        _ => 0,
    }
}

/// The address each server listens on out of the box.
pub fn default_endpoint(backend: &str) -> &'static str {
    match backend {
        "llamacpp" => "http://127.0.0.1:8080",
        _ => "http://127.0.0.1:11434",
    }
}

/// A suggestion's command as the person should type it: with `sudo` when it
/// needs privilege they do not have, the same way `oracle doctor` prints it.
pub fn shown_command(s: &Suggestion, root: bool) -> Option<String> {
    let cmd = s.command.as_ref()?;
    Some(if s.needs_root && !root {
        format!("sudo {cmd}")
    } else {
        cmd.clone()
    })
}

pub fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "Critical",
        Severity::Warning => "Warning",
        Severity::Note => "Note",
    }
}

/// The icon for the worst severity present, or for a clean machine.
pub fn severity_icon(worst: Option<Severity>) -> &'static str {
    match worst {
        None => "object-select-symbolic",
        Some(Severity::Critical) => "dialog-error-symbolic",
        Some(Severity::Warning) => "dialog-warning-symbolic",
        Some(Severity::Note) => "dialog-information-symbolic",
    }
}

pub const SEVERITY_CLASSES: [&str; 4] = ["sev-clean", "sev-critical", "sev-warning", "sev-note"];

pub fn severity_class(worst: Option<Severity>) -> &'static str {
    match worst {
        None => "sev-clean",
        Some(Severity::Critical) => "sev-critical",
        Some(Severity::Warning) => "sev-warning",
        Some(Severity::Note) => "sev-note",
    }
}

/// What to tell someone whose question could not reach a model. The command
/// line points at `oracle setup`; here the same settings are a page away.
pub fn no_model_advice(cfg: &Config) -> String {
    let backend = cfg.model.backend.trim();
    if backend.is_empty() || backend == "none" {
        return "No model is configured, so Oracle can run its checks but cannot answer in \
                prose. Settings has the model options."
            .into();
    }
    format!(
        "Oracle could not reach the {backend} server at {}. The checks still work without \
         it, and Settings can point Oracle somewhere else.",
        cfg.model.endpoint
    )
}

/// An address as Oracle stores it: trimmed, and with `http://` in front when
/// none was typed, so the saved file reads the way the defaults do.
pub fn normalise_endpoint(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t.contains("://") {
        t.to_string()
    } else {
        format!("http://{t}")
    }
}

/// Why Oracle will not use the server in `cfg`, in words that point at this
/// app's controls rather than at the config file. `None` means it may try.
///
/// The rule is the core's -- `Config::endpoint` enforces it on every call --
/// but its message names a TOML key, which is no help to someone looking at a
/// switch.
pub fn endpoint_problem(cfg: &Config) -> Option<String> {
    let backend = cfg.model.backend.trim().to_ascii_lowercase();
    if backend.is_empty() || backend == "none" {
        return None;
    }
    let url = match oracle::http::Url::parse(&cfg.model.endpoint) {
        Ok(u) => u,
        Err(e) => return Some(format!("The address is not usable: {e}.")),
    };
    if !cfg.privacy.allow_remote_endpoint && !url.is_loopback() {
        return Some(format!(
            "{} is not on this machine, so Oracle will not send anything to it. To use it \
             anyway, turn on “Allow a model on another machine” under Privacy.",
            url.origin()
        ));
    }
    None
}

/// Whether the model part of two configs differs: what decides which server a
/// question would go to.
pub fn model_settings_differ(a: &Config, b: &Config) -> bool {
    a.model.backend != b.model.backend
        || a.model.endpoint != b.model.endpoint
        || a.model.name != b.model.name
        || a.privacy.allow_remote_endpoint != b.privacy.allow_remote_endpoint
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(severities: &[Severity]) -> State {
        State {
            findings: severities
                .iter()
                .enumerate()
                .map(|(i, s)| Finding::new(&format!("f{i}"), *s, "t"))
                .collect(),
            scanned_once: true,
            ..Default::default()
        }
    }

    #[test]
    fn each_filter_admits_its_severities_and_everything_worse() {
        let st = state_with(&[Severity::Critical, Severity::Warning, Severity::Note]);
        assert_eq!(st.visible(Filter::All).len(), 3);
        assert_eq!(st.visible(Filter::WarningsAndWorse).len(), 2);
        assert_eq!(st.visible(Filter::CriticalOnly).len(), 1);
    }

    #[test]
    fn the_worst_severity_is_the_most_serious_one() {
        assert_eq!(
            state_with(&[Severity::Note, Severity::Warning]).worst(),
            Some(Severity::Warning)
        );
        assert_eq!(state_with(&[]).worst(), None);
    }

    #[test]
    fn a_fresh_state_has_not_looked_and_has_no_model_yet() {
        let st = State::default();
        assert!(
            !st.scanned_once,
            "it must not look like a clean machine yet"
        );
        assert!(matches!(st.model, ModelState::Checking));
        assert!(!st.model.ready());
    }

    #[test]
    fn a_command_that_needs_privilege_is_shown_with_sudo_unless_already_root() {
        let s = Suggestion::root_cmd("migrate", "pacman-db-upgrade");
        assert_eq!(
            shown_command(&s, false).as_deref(),
            Some("sudo pacman-db-upgrade")
        );
        assert_eq!(
            shown_command(&s, true).as_deref(),
            Some("pacman-db-upgrade")
        );
        assert_eq!(shown_command(&Suggestion::new("look"), false), None);
    }

    #[test]
    fn areas_switched_off_in_settings_are_reported_as_not_checked() {
        let mut cfg = Config::default();
        assert!(skipped_areas(&cfg).is_empty());
        set_area(&mut cfg, Area::Logs, false);
        set_area(&mut cfg, Area::Network, false);
        assert_eq!(skipped_areas(&cfg), vec!["network", "logs"]);
        assert!(!area_enabled(&cfg, Area::Logs));
        assert!(area_enabled(&cfg, Area::Storage));
    }

    #[test]
    fn every_backend_spelling_the_command_line_accepts_finds_its_row() {
        assert_eq!(backend_index("ollama"), 0);
        assert_eq!(backend_index("llama.cpp"), 1);
        assert_eq!(backend_index("openai"), 1);
        assert_eq!(backend_index("none"), 2);
        assert_eq!(backend_index(""), 2);
        assert_eq!(
            BACKENDS[backend_index("llama-server") as usize].0,
            "llamacpp"
        );
    }

    #[test]
    fn a_model_that_is_up_but_empty_is_not_ready() {
        let up = |selected: Option<&str>| ModelState::Checked {
            backend: "ollama",
            availability: Availability {
                reachable: true,
                endpoint: "http://127.0.0.1:11434".into(),
                models: vec![],
                selected: selected.map(String::from),
                detail: None,
            },
        };
        assert!(!up(None).ready());
        assert!(up(Some("qwen2.5:3b-instruct")).ready());
        assert!(
            up(Some("qwen2.5:3b-instruct"))
                .describe()
                .contains("qwen2.5")
        );
    }

    #[test]
    fn an_address_typed_without_a_scheme_is_given_one() {
        assert_eq!(
            normalise_endpoint(" 192.168.1.50:11434 "),
            "http://192.168.1.50:11434"
        );
        assert_eq!(
            normalise_endpoint("http://127.0.0.1:8080"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(normalise_endpoint("https://x"), "https://x");
        assert_eq!(normalise_endpoint(""), "");
    }

    #[test]
    fn a_server_on_another_machine_is_refused_until_the_switch_is_on() {
        let mut cfg = Config::default();
        cfg.model.endpoint = "http://192.168.1.50:11434".into();
        let problem = endpoint_problem(&cfg).expect("must refuse");
        assert!(
            problem.contains("Allow a model on another machine"),
            "got {problem}"
        );
        assert!(!problem.contains("allow_remote_endpoint"), "got {problem}");

        cfg.privacy.allow_remote_endpoint = true;
        assert_eq!(endpoint_problem(&cfg), None);
    }

    #[test]
    fn a_local_server_or_no_server_has_no_problem() {
        let mut cfg = Config::default();
        assert_eq!(endpoint_problem(&cfg), None);
        cfg.model.endpoint = "localhost:8080".into();
        assert_eq!(endpoint_problem(&cfg), None);
        cfg.model.backend = "none".into();
        cfg.model.endpoint = "https://anything".into();
        assert_eq!(endpoint_problem(&cfg), None);
    }

    #[test]
    fn https_is_accepted_but_does_not_make_a_remote_server_local() {
        let mut cfg = Config::default();
        cfg.model.endpoint = "https://127.0.0.1:11434".into();
        assert_eq!(
            endpoint_problem(&cfg),
            None,
            "https on this machine is fine"
        );

        cfg.model.endpoint = "https://192.168.1.50".into();
        let problem = endpoint_problem(&cfg).expect("off-machine https must still be refused");
        assert!(
            problem.contains("https://192.168.1.50:443"),
            "got {problem}"
        );
        assert!(
            problem.contains("Allow a model on another machine"),
            "got {problem}"
        );
    }

    #[test]
    fn only_the_model_settings_count_as_a_different_server() {
        let saved = Config::default();
        let mut form = saved.clone();
        form.context.logs = false;
        assert!(!model_settings_differ(&saved, &form));
        form.model.endpoint = "http://127.0.0.1:8080".into();
        assert!(model_settings_differ(&saved, &form));
    }

    #[test]
    fn a_named_model_the_server_lacks_is_explained_with_what_it_has() {
        // The real case: `ornith-1.5:latest` asked for, `:35B` and `:9b` present.
        let state = ModelState::Checked {
            backend: "ollama",
            availability: Availability {
                reachable: true,
                endpoint: "https://gpt.example.com:443".into(),
                models: vec!["ornith-1.5:35B".into(), "ornith-1.5:9b".into()],
                selected: None,
                detail: Some(
                    "The configured model \"ornith-1.5:latest\" is not installed on this server."
                        .into(),
                ),
            },
        };
        let text = state.describe();
        assert!(text.contains("ornith-1.5:latest"), "got {text}");
        assert!(text.contains("ornith-1.5:35B, ornith-1.5:9b"), "got {text}");
        assert!(!text.contains("has no model"), "got {text}");
    }

    #[test]
    fn a_long_model_list_is_cut_short_and_counted() {
        let state = ModelState::Checked {
            backend: "ollama",
            availability: Availability {
                reachable: true,
                endpoint: "http://127.0.0.1:11434".into(),
                models: (0..11).map(|i| format!("m{i}")).collect(),
                selected: None,
                detail: None,
            },
        };
        let text = state.describe();
        assert!(text.contains("m7, and 3 more"), "got {text}");
        assert!(!text.contains("m8"), "got {text}");
    }

    #[test]
    fn the_advice_without_a_model_points_at_settings_not_a_terminal() {
        let mut cfg = Config::default();
        assert!(no_model_advice(&cfg).contains("11434"));
        cfg.model.backend = "none".into();
        assert!(no_model_advice(&cfg).contains("Settings"));
        assert!(!no_model_advice(&cfg).contains("oracle setup"));
    }
}
