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
    /// Error lines this boot wrote to this service's log, when it has one.
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
        (this_boot_errors(&candidate, 400, 4), Some(candidate), age)
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

/// Error-looking lines this boot wrote to a log, oldest first.
///
/// raven-init appends to the same file across boots, so the tail of a log is
/// often last week's. Reporting those lines as happening now is the kind of
/// wrong answer that teaches people to ignore the report, so anything that
/// cannot be placed in this boot is left out.
///
/// Matching on words rather than on a log format keeps this working across the
/// several shapes of line the Raven daemons emit. The precision comes from
/// three constraints rather than from a parser: the marker has to stand as its
/// own word, lines that are really command invocations are rejected outright,
/// and near-identical repeats collapse to one.
pub fn this_boot_errors(path: impl AsRef<Path>, window: usize, keep: usize) -> Vec<String> {
    let path = path.as_ref();
    let boot = crate::sys::boot_time();
    // A log nothing has written since boot holds nothing from this boot.
    if let (Some(boot), Some(modified)) = (boot, crate::sys::modified_epoch(path))
        && modified < boot
    {
        return Vec::new();
    }
    let Some(lines) = crate::sys::tail(path, window) else {
        return Vec::new();
    };
    let first = crate::sys::first_line(path);
    let mut hits: Vec<String> = current_run(lines, first.as_deref(), boot)
        .into_iter()
        .filter(|l| is_error_line(l))
        .map(|l| crate::sys::truncate_line(&l, MAX_LOG_LINE))
        .collect();
    // Keep the latest of each repeat, so the evidence shows when it last
    // happened rather than when it first did.
    hits.reverse();
    dedupe_similar(&mut hits);
    hits.reverse();
    // Keep the most recent few; a wall of repeated errors helps nobody.
    let start = hits.len().saturating_sub(keep);
    hits.drain(..start);
    hits
}

/// How far behind the boot time a line's timestamp may fall and still count
/// as this boot, for a clock that was stepped after the kernel started.
const CLOCK_SLACK_SECONDS: u64 = 5;

/// The part of a log's tail written since this boot, as far as the log lets
/// that be told.
///
/// Three signals, in order of how far they can be trusted: a timestamp at the
/// start of a line, compared with the boot time; the pid in a `name[pid]:`
/// prefix, which changes whenever the daemon starts again; and the line a
/// daemon writes first every time it starts, whose last appearance marks its
/// latest start. A log with none of these is kept whole, which is safe because
/// its modified time has already been checked against the boot.
pub fn current_run(lines: Vec<String>, first_line: Option<&str>, boot: Option<u64>) -> Vec<String> {
    let start = run_start(&lines, first_line, boot);
    lines.into_iter().skip(start).collect()
}

fn run_start(lines: &[String], first_line: Option<&str>, boot: Option<u64>) -> usize {
    if let Some(boot) = boot {
        let stamped: Vec<(usize, u64)> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| leading_timestamp(l).map(|t| (i, t)))
            .collect();
        if !stamped.is_empty() {
            return stamped
                .iter()
                .rev()
                .find(|(_, t)| t + CLOCK_SLACK_SECONDS < boot)
                .map_or(0, |(i, _)| i + 1);
        }
    }

    let tagged: Vec<(usize, u32)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| tagged_pid(l).map(|p| (i, p)))
        .collect();
    if let Some(&(_, latest)) = tagged.last() {
        // The last unbroken run of the latest pid, since pids repeat across
        // boots.
        return tagged
            .iter()
            .rev()
            .find(|(_, p)| *p != latest)
            .map_or(0, |(i, _)| i + 1);
    }

    // A clock counting from the start of the boot or of the daemon, the way
    // raven-init and seatd stamp their lines, goes backwards when either
    // starts again.
    let elapsed: Vec<(usize, f64)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| elapsed_stamp(l).map(|t| (i, t)))
        .collect();
    if let Some(reset) = elapsed.windows(2).rev().find(|w| w[1].1 < w[0].1) {
        return reset[1].0;
    }

    // A banner that is itself an error would cut the log at its latest error
    // and hide everything before it.
    if let Some(banner) = first_line
        .map(str::trim_end)
        .filter(|b| !b.trim().is_empty() && !is_error_line(b))
        && let Some(i) = lines.iter().rposition(|l| l.trim_end() == banner)
    {
        return i;
    }
    0
}

/// The pid in a `name[pid]:` prefix, the way dbus and bluetoothd write it.
fn tagged_pid(line: &str) -> Option<u32> {
    let head = line.split_whitespace().next()?.strip_suffix("]:")?;
    let (name, pid) = head.split_once('[')?;
    if name.is_empty() {
        return None;
    }
    digits(pid)?.try_into().ok()
}

/// A stamp counting seconds from some start rather than from the epoch:
/// `[    4.094]` as the kernel and raven-init write it near the start of a
/// line, or `00:00:05.940` as seatd does.
fn elapsed_stamp(line: &str) -> Option<f64> {
    let decimal = |s: &str| {
        let (whole, fraction) = s.split_once('.')?;
        digits(whole)?;
        digits(fraction)?;
        s.parse::<f64>().ok()
    };

    for piece in line.split('[').skip(1).take(3) {
        if let Some((inner, _)) = piece.split_once(']')
            && let Some(t) = decimal(inner.trim_start())
        {
            return Some(t);
        }
    }

    let first = line.split_whitespace().next()?;
    let mut parts = first.splitn(3, ':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    Some((digits(h)? * 3600 + digits(m)? * 60) as f64 + decimal(s)?)
}

/// An RFC 3339 timestamp at the start of a line, in seconds since the epoch,
/// with or without the bracket and colour codes the Raven daemons put around
/// it.
fn leading_timestamp(line: &str) -> Option<u64> {
    let plain = strip_ansi(line);
    let s = plain.trim_start().trim_start_matches('[');
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let field = |from: usize, to: usize| s.get(from..to).and_then(digits);
    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    // Every byte before 19 was checked to be ASCII, so this slices cleanly.
    let mut rest = &s[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        rest = fraction.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match *rest.as_bytes().first()? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            if rest.as_bytes().get(3) != Some(&b':') {
                return None;
            }
            let secs = digits(rest.get(1..3)?)? * 3600 + digits(rest.get(4..6)?)? * 60;
            if sign == b'+' { secs } else { -secs }
        }
        _ => return None,
    };

    let epoch =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(epoch).ok()
}

fn digits(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Days from 1970-01-01 to a date in the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A line with its terminal colour codes removed.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
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

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn timestamps_from_before_this_boot_are_dropped() {
        // 2026-09-15T17:14:34Z
        let boot = 1_789_492_474;
        let log = lines(
            "[2026-09-14T13:59:23Z WARN  raven_timed] Sync failed: last boot\n\
             [2026-09-15T15:37:25Z WARN  raven_timed] Sync failed: also last boot\n\
             [2026-09-15T17:14:39Z INFO  raven_timed] raven-timed: zone America/New_York\n\
             [2026-09-15T17:17:09Z WARN  raven_timed] Sync failed: this boot",
        );
        let run = current_run(log, None, Some(boot));
        assert_eq!(run.len(), 2, "got {run:?}");
        assert!(run[1].ends_with("this boot"));
    }

    #[test]
    fn timestamps_are_read_through_colour_codes_and_offsets() {
        assert_eq!(leading_timestamp("1970-01-01T00:00:00Z x"), Some(0));
        assert_eq!(
            leading_timestamp("[2000-03-01T00:00:00Z x"),
            Some(951_868_800)
        );
        assert_eq!(
            leading_timestamp("2000-03-01T01:00:00.123+01:00 x"),
            Some(951_868_800)
        );
        assert_eq!(
            leading_timestamp("\u{1b}[2m2026-09-15T17:14:34.026075Z\u{1b}[0m \u{1b}[32m INFO"),
            Some(1_789_492_474)
        );
        assert_eq!(
            leading_timestamp("00:00:05.940 [INFO] [seatd/seat.c:584]"),
            None
        );
        assert_eq!(
            leading_timestamp("raven-fstrim: trimming (2026-09-14 10:04)"),
            None
        );
    }

    #[test]
    fn only_the_latest_pid_counts_even_when_an_old_boot_reused_it() {
        let log = lines(
            "dbus-daemon[164]: activation failed: two boots ago\n\
             dbus-daemon[161]: activation failed: last boot\n\
             dbus[164]: Unknown username \"polkitd\"\n\
             dbus-daemon[164]: activation failed: this boot",
        );
        let run = current_run(log, None, Some(1_789_492_474));
        assert_eq!(run.len(), 2, "got {run:?}");
        assert!(run[1].ends_with("this boot"));
    }

    #[test]
    fn an_uptime_clock_going_backwards_marks_the_boot() {
        let log = lines(
            "[raven-init] [ 5016.714] INFO: Re-executed as PID 1\n\
             [raven-init] [ 5016.937] ERROR: network exited: last boot\n\
             [raven-init] [    3.893] INFO: Mounted /boot/efi\n\
             [raven-init] [    9.100] ERROR: network exited: this boot",
        );
        let run = current_run(log, None, None);
        assert_eq!(run.len(), 2, "got {run:?}");
        assert!(run[1].ends_with("this boot"));

        let seatd = lines(
            "00:00:05.940 [ERROR] [seatd/seat.c:584] last start\n\
             00:00:00.000 [INFO] [seatd/seat.c:48] Created VT-bound seat seat0\n\
             00:00:01.200 [ERROR] [seatd/seat.c:584] this start",
        );
        assert_eq!(current_run(seatd, None, None).len(), 2);

        // Chromium's `[pid:tid:date/time.micros:LEVEL]` is not an elapsed clock.
        assert_eq!(
            elapsed_stamp("[5866:1:0915/142324.368502:ERROR:puffpatch.cc:1] x"),
            None
        );
    }

    #[test]
    fn a_repeated_startup_line_marks_the_latest_start() {
        let log = lines(
            "cawd: connection failed: before the restart\n\
             cawd: listening on /run/caw/caw.sock\n\
             cawd: connection failed: since the restart",
        );
        let run = current_run(log, Some("cawd: listening on /run/caw/caw.sock"), None);
        assert_eq!(run.len(), 2, "got {run:?}");
        assert!(run[1].ends_with("since the restart"));
    }

    #[test]
    fn a_first_line_that_is_an_error_is_not_taken_for_a_startup_line() {
        let log = lines("cawd: connection failed\ncawd: link up\ncawd: connection failed");
        let run = current_run(log.clone(), Some("cawd: connection failed"), None);
        assert_eq!(run, log);
    }

    #[test]
    fn a_log_with_nothing_to_place_it_is_kept_whole() {
        let log = lines("rvnd: failed to bind\nrvnd: retrying");
        assert_eq!(current_run(log.clone(), None, Some(1_789_492_474)), log);
    }

    #[test]
    fn evidence_is_this_boots_latest_repeat() {
        let dir = std::env::temp_dir().join(format!("oracle-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dbus.log");
        std::fs::write(
            &path,
            "dbus-daemon[9164]: activation failed 0\n\
             dbus-daemon[9161]: started\n\
             dbus-daemon[9161]: activation failed 1\n\
             dbus-daemon[9161]: activation failed 2\n",
        )
        .unwrap();
        let errors = this_boot_errors(&path, 400, 4);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(errors, vec!["dbus-daemon[9161]: activation failed 2"]);
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
