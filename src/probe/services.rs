//! Services: what is meant to be running, and what actually is.
//!
//! This is the probe that earns Oracle its keep on Raven Linux. `raven-init`
//! services carry `critical = false`, which means a service whose `exec` does
//! not exist fails on every boot and says nothing -- the exact failure
//! described in RavenLinux's own notes, where `raven-dhcp` was configured,
//! enabled, and never built, and nobody found out from the running system.
//!
//! Checking a declared `exec` against the filesystem costs a `stat` and turns
//! that class of bug from invisible into obvious. Everything else here is in
//! the same spirit: compare what the configuration promises against what the
//! machine can actually do.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Default, Serialize)]
pub struct Services {
    /// `raven-init`, `systemd`, or neither.
    pub manager: String,
    pub raven: Vec<RavenService>,
    /// Units systemd itself reports as failed.
    pub systemd_failed: Vec<SystemdUnit>,
    /// Set when the service directory exists but could not be read.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RavenService {
    pub name: String,
    pub description: String,
    pub exec: String,
    pub enabled: bool,
    pub critical: bool,
    pub restart: bool,
    pub after: Vec<String>,
    /// Where the definition came from, for evidence.
    pub source: String,
    /// Whether `exec` exists and is executable right now.
    pub exec_present: bool,
    /// A `ready_path` the service promises to create, and whether it is there.
    pub ready_path: Option<String>,
    pub ready_present: Option<bool>,
    /// The tail of this service's log, when it has one.
    pub log_errors: Vec<String>,
    pub log_path: Option<String>,
    /// How long ago the log was last written. Errors in a log nothing has
    /// touched for weeks are history, not a problem to act on.
    pub log_age_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemdUnit {
    pub unit: String,
    pub state: String,
}

/// The subset of a `raven-init` service definition Oracle needs.
///
/// `deny_unknown_fields` is deliberately *not* set: raven-init grows fields,
/// and a probe that refuses to read a service because it learned a new key
/// would be worse than useless on exactly the systems it is meant to help.
#[derive(Debug, Deserialize)]
struct ServiceFile {
    #[serde(default)]
    services: Vec<ServiceDef>,
}

#[derive(Debug, Deserialize)]
struct ServiceDef {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    exec: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    critical: bool,
    #[serde(default)]
    restart: bool,
    #[serde(default)]
    after: Vec<String>,
    #[serde(default)]
    ready_path: Option<String>,
}

const RAVEN_ETC: &str = "/etc/raven";
const RAVEN_LOGS: &str = "/var/log/raven";

pub fn probe() -> Services {
    let mut s = Services {
        manager: crate::probe::init_system(),
        ..Default::default()
    };

    if Path::new(RAVEN_ETC).is_dir() {
        collect_raven(&mut s);
    }
    if crate::sys::have("systemctl") {
        collect_systemd(&mut s);
    }
    s
}

fn collect_raven(s: &mut Services) {
    let mut files: Vec<String> = Vec::new();
    let main = format!("{RAVEN_ETC}/init.toml");
    if Path::new(&main).is_file() {
        files.push(main);
    }

    let dir = format!("{RAVEN_ETC}/init.d");
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            let mut found: Vec<String> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            // Stable order, so two runs on an unchanged machine agree.
            found.sort();
            files.extend(found);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => s.error = Some(format!("cannot read {dir}: {e}")),
    }

    for path in files {
        let Some(text) = crate::sys::read(&path) else {
            continue;
        };
        let parsed: ServiceFile = match toml::from_str(&text) {
            Ok(p) => p,
            Err(e) => {
                // A service file that does not parse is itself a finding: on
                // Raven that is a boot-time hard error.
                s.error = Some(format!("{path} does not parse: {e}"));
                continue;
            }
        };
        for def in parsed.services {
            s.raven.push(inspect(def, &path));
        }
    }
}

fn inspect(def: ServiceDef, source: &str) -> RavenService {
    let exec_present = !def.exec.is_empty() && crate::sys::which(&def.exec).is_some();
    let ready_present = def.ready_path.as_ref().map(|p| Path::new(p).exists());

    let candidate = format!("{RAVEN_LOGS}/{}.log", def.name);
    let (log_errors, log_path, log_age_seconds) = if Path::new(&candidate).is_file() {
        let age = std::fs::metadata(&candidate)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs());
        (recent_errors(&candidate), Some(candidate), age)
    } else {
        (Vec::new(), None, None)
    };

    RavenService {
        name: def.name,
        description: def.description,
        exec: def.exec,
        enabled: def.enabled,
        critical: def.critical,
        restart: def.restart,
        after: def.after,
        source: source.to_string(),
        exec_present,
        ready_path: def.ready_path,
        ready_present,
        log_errors,
        log_path,
        log_age_seconds,
    }
}

/// Error-looking lines from the tail of a service log.
///
/// Matching on words rather than on a log format keeps this working across the
/// several shapes of line the Raven daemons emit. The precision comes from
/// three constraints rather than from a parser: the marker has to stand as its
/// own word, lines that are really command invocations are rejected outright,
/// and near-identical repeats collapse to one.
fn recent_errors(path: &str) -> Vec<String> {
    let Some(lines) = crate::sys::tail(path, 400) else {
        return Vec::new();
    };
    let mut hits: Vec<String> = lines
        .into_iter()
        .filter(|l| is_error_line(l))
        .map(|l| crate::sys::truncate_line(&l, MAX_LOG_LINE))
        .collect();
    dedupe_similar(&mut hits);
    // Keep the most recent few; a wall of repeated errors helps nobody.
    let start = hits.len().saturating_sub(4);
    hits.drain(..start);
    hits
}

/// The longest log line worth putting in a report.
pub const MAX_LOG_LINE: usize = 220;

const MARKERS: [&str; 12] = [
    "error", "failed", "failure", "cannot", "unable", "refused", "denied", "panic", "fatal",
    "timeout", "missing", "invalid",
];

/// Phrases that mention failure in order to deny it.
const ALL_CLEAR: [&str; 5] = [
    "0 failed",
    "no errors",
    "failed = 0",
    "without error",
    "no failures",
];

pub fn is_error_line(line: &str) -> bool {
    let l = line.to_ascii_lowercase();

    if ALL_CLEAR.iter().any(|e| l.contains(e)) {
        return false;
    }
    // A build or command line is not a log of a failure, even though compiler
    // flags are full of the word "error". `-Werror=format-security` in a
    // package build once produced a two-thousand-character "finding".
    if looks_like_a_command(&l) {
        return false;
    }
    // Multi-word phrases have their own boundaries already.
    if l.contains("no such file") || l.contains("permission denied") || l.contains("can't") {
        return true;
    }
    MARKERS.iter().any(|m| contains_word(&l, m))
}

/// Whether `needle` appears in `haystack` bounded by non-alphanumerics.
///
/// This is what separates a line reporting an error from a line that merely
/// contains `-Werror` or the word `errorless`.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    haystack.match_indices(needle).any(|(at, _)| {
        let before_ok = at == 0 || !is_wordish(bytes[at - 1]);
        let end = at + needle.len();
        // A trailing suffix is allowed, so `failed`, `failing` and `errors`
        // all count; a leading one is not, so `-Werror` does not.
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_digit();
        before_ok && after_ok
    })
}

fn is_wordish(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether a line is a command being echoed rather than a message.
fn looks_like_a_command(line: &str) -> bool {
    // Several distinct option flags, or an enormous line, means a build log.
    let flags = line
        .split_whitespace()
        .filter(|w| w.starts_with('-'))
        .count();
    flags >= 4 || line.len() > 600
}

/// Collapse repeats that differ only in numbers -- timestamps, PIDs, retry
/// counters -- keeping the first of each.
pub fn dedupe_similar(lines: &mut Vec<String>) {
    let mut seen: std::collections::HashSet<String> = Default::default();
    lines.retain(|l| seen.insert(shape_of(l)));
}

/// A line with every digit flattened, so two repeats compare equal.
pub fn shape_of(line: &str) -> String {
    line.chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect()
}

fn collect_systemd(s: &mut Services) {
    let Some(out) = crate::sys::quick(
        "systemctl",
        &["--failed", "--no-legend", "--no-pager", "--plain"],
    ) else {
        return;
    };
    let Some(text) = out.text() else { return };
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 3 {
            s.systemd_failed.push(SystemdUnit {
                unit: cols[0].to_string(),
                state: cols[2..].join(" "),
            });
        }
    }
}

impl Services {
    /// Enabled services whose executable is missing.
    ///
    /// On a `critical = false` service this is a failure nobody is told about,
    /// which is the whole reason this probe exists.
    pub fn silent_failures(&self) -> Vec<&RavenService> {
        self.raven
            .iter()
            .filter(|s| s.enabled && !s.exec.is_empty() && !s.exec_present)
            .collect()
    }

    /// Services that promised a readiness path and have not produced it.
    pub fn not_ready(&self) -> Vec<&RavenService> {
        self.raven
            .iter()
            .filter(|s| s.enabled && s.ready_present == Some(false))
            .collect()
    }

    /// Enabled services that depend on something disabled or unknown.
    pub fn broken_dependencies(&self) -> Vec<(&RavenService, String)> {
        let mut out = Vec::new();
        for s in self.raven.iter().filter(|s| s.enabled) {
            for dep in &s.after {
                match self.raven.iter().find(|o| &o.name == dep) {
                    Some(target) if !target.enabled => {
                        out.push((s, format!("{dep} is defined but disabled")))
                    }
                    None => out.push((s, format!("{dep} is not defined anywhere"))),
                    _ => {}
                }
            }
        }
        out
    }

    /// Enabled services whose log has errors in it and has been written
    /// recently enough for those errors to still be happening.
    pub fn logging_errors(&self, within_seconds: u64) -> Vec<&RavenService> {
        self.raven
            .iter()
            .filter(|s| s.enabled && !s.log_errors.is_empty())
            .filter(|s| {
                s.log_age_seconds
                    .map(|a| a <= within_seconds)
                    .unwrap_or(false)
            })
            .collect()
    }

    pub fn enabled_count(&self) -> usize {
        self.raven.iter().filter(|s| s.enabled).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(name: &str, exec_present: bool, enabled: bool) -> RavenService {
        RavenService {
            name: name.into(),
            description: String::new(),
            exec: format!("/usr/bin/{name}"),
            enabled,
            critical: false,
            restart: true,
            after: Vec::new(),
            source: "/etc/raven/init.toml".into(),
            exec_present,
            ready_path: None,
            ready_present: None,
            log_errors: Vec::new(),
            log_path: None,
            log_age_seconds: None,
        }
    }

    #[test]
    fn an_enabled_service_with_no_binary_is_a_silent_failure() {
        let s = Services {
            manager: "raven-init".into(),
            raven: vec![svc("raven-dhcp", false, true), svc("rvnd", true, true)],
            systemd_failed: vec![],
            error: None,
        };
        let found = s.silent_failures();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "raven-dhcp");
    }

    #[test]
    fn a_disabled_service_with_no_binary_is_not_a_problem() {
        let s = Services {
            manager: "raven-init".into(),
            raven: vec![svc("postinstall", false, false)],
            systemd_failed: vec![],
            error: None,
        };
        assert!(s.silent_failures().is_empty());
    }

    #[test]
    fn depending_on_a_disabled_service_is_reported() {
        let mut a = svc("store", true, true);
        a.after = vec!["rvnd".into()];
        let s = Services {
            manager: "raven-init".into(),
            raven: vec![a, svc("rvnd", true, false)],
            systemd_failed: vec![],
            error: None,
        };
        let broken = s.broken_dependencies();
        assert_eq!(broken.len(), 1);
        assert!(broken[0].1.contains("disabled"));
    }

    #[test]
    fn error_lines_are_recognised_without_catching_the_all_clear() {
        assert!(is_error_line("rvnd: failed to bind /run/rvn/ctl"));
        assert!(is_error_line("exec: No such file or directory"));
        assert!(is_error_line(
            "dbus-daemon: Activated service failed: Permission denied"
        ));
        assert!(!is_error_line("startup complete, 0 failed"));
        assert!(!is_error_line("listening on /run/rvn/ctl"));
    }

    #[test]
    fn a_compiler_flag_is_not_an_error_report() {
        // This line really did become a finding: -Werror contains "error".
        let build = "/usr/sbin/c++ -O2 -pipe -Wformat -Werror=format-security -fno-plt                      -o thing.o thing.cpp";
        assert!(
            !is_error_line(build),
            "a build command line is not a failure, however many times it says error"
        );
    }

    #[test]
    fn a_word_boundary_separates_a_report_from_a_flag() {
        assert!(contains_word("connection refused by peer", "refused"));
        assert!(contains_word("two errors occurred", "error"));
        assert!(!contains_word("-werror=format-security", "error"));
    }

    #[test]
    fn identical_repeats_differing_only_in_a_pid_collapse() {
        let mut lines = vec![
            "dbus-daemon[131]: activation failed".to_string(),
            "dbus-daemon[142]: activation failed".to_string(),
            "dbus-daemon[151]: activation failed".to_string(),
            "dbus-daemon[151]: something else entirely".to_string(),
        ];
        dedupe_similar(&mut lines);
        assert_eq!(lines.len(), 2, "got {lines:?}");
    }

    #[test]
    fn a_stale_log_is_not_reported_as_a_live_problem() {
        let mut old = svc("timed", true, true);
        old.log_errors = vec!["sync failed".into()];
        old.log_age_seconds = Some(40 * 86_400);

        let mut fresh = svc("dbus", true, true);
        fresh.log_errors = vec!["activation failed".into()];
        fresh.log_age_seconds = Some(600);

        let s = Services {
            manager: "raven-init".into(),
            raven: vec![old, fresh],
            systemd_failed: vec![],
            error: None,
        };
        let live = s.logging_errors(7 * 86_400);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].name, "dbus");
    }

    #[test]
    fn a_real_raven_service_file_parses() {
        let text = r#"
[[services]]
name = "rvnd"
description = "Package installs for members of wheel, without sudo"
exec = "/usr/bin/rvnd"
args = []
after = ["udev"]
ready_path = "/run/rvn/ctl"
ready_timeout = 5
restart = true
enabled = true
critical = false
"#;
        let parsed: ServiceFile = toml::from_str(text).expect("the shipped format must parse");
        assert_eq!(parsed.services.len(), 1);
        assert_eq!(parsed.services[0].name, "rvnd");
        assert_eq!(
            parsed.services[0].ready_path.as_deref(),
            Some("/run/rvn/ctl")
        );
        assert!(parsed.services[0].enabled);
    }

    #[test]
    fn an_unknown_future_field_does_not_break_the_parse() {
        let text = r#"
[[services]]
name = "future"
exec = "/usr/bin/future"
enabled = true
some_field_added_next_year = 42
"#;
        let parsed: ServiceFile = toml::from_str(text).expect("forwards compatible");
        assert_eq!(parsed.services[0].name, "future");
    }
}
