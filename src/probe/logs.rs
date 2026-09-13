//! Logs: the recent past, in the words of whatever wrote it.
//!
//! Oracle reads three places, in the order a Raven system is likely to have
//! them: the per-service logs `raven-init` writes, the kernel ring buffer, and
//! the systemd journal when there is one. Only error-looking lines survive,
//! and only the most recent handful of those, because the value of a log in a
//! diagnosis is the two lines before the failure and not the ten thousand
//! before those.

use serde::Serialize;
use std::path::Path;

#[derive(Debug, Default, Serialize)]
pub struct Logs {
    pub raven: Vec<LogSource>,
    pub kernel: Vec<String>,
    pub journal: Vec<String>,
    /// Set when a log exists but this account may not read it.
    pub unreadable: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogSource {
    pub service: String,
    pub path: String,
    pub errors: Vec<String>,
    pub modified_seconds_ago: Option<u64>,
}

const RAVEN_LOGS: &str = "/var/log/raven";

pub fn probe(max_lines: usize) -> Logs {
    let max_lines = max_lines.clamp(5, 400);
    let mut l = Logs::default();

    if Path::new(RAVEN_LOGS).is_dir() {
        collect_raven(&mut l, max_lines);
    }
    l.kernel = kernel_errors(max_lines);
    l.journal = journal_errors(max_lines);
    l
}

fn collect_raven(l: &mut Logs, max_lines: usize) {
    let Ok(entries) = std::fs::read_dir(RAVEN_LOGS) else {
        l.unreadable.push(RAVEN_LOGS.to_string());
        return;
    };

    let mut paths: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "log").unwrap_or(false))
        .collect();
    paths.sort();

    for path in paths {
        let service = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        let Some(lines) = crate::sys::tail(&path, max_lines * 5) else {
            l.unreadable.push(path.to_string_lossy().into_owned());
            continue;
        };

        let mut errors: Vec<String> = lines
            .into_iter()
            .filter(|line| crate::probe::services::is_error_line(line))
            .map(|line| crate::sys::truncate_line(&line, crate::probe::services::MAX_LOG_LINE))
            .collect();
        dedupe_keeping_order(&mut errors);
        let start = errors.len().saturating_sub(6);
        errors.drain(..start);

        if errors.is_empty() {
            continue;
        }

        let modified_seconds_ago = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs());

        l.raven.push(LogSource {
            service,
            path: path.to_string_lossy().into_owned(),
            errors,
            modified_seconds_ago,
        });
    }
}

/// Kernel messages worth a human's attention.
///
/// The filter is narrower than "contains error": the ring buffer is full of
/// noise that alarms people for no reason. Firmware that failed to load, a
/// process the OOM killer took, a disk that reported an I/O error, and a
/// filesystem remounted read-only are all things that explain a symptom
/// somebody is actually experiencing.
fn kernel_errors(max_lines: usize) -> Vec<String> {
    let out = crate::sys::quick("dmesg", &["--level=err,crit,alert,emerg", "--notime"])
        .or_else(|| crate::sys::quick("dmesg", &["-l", "err,crit,alert,emerg"]))
        .or_else(|| crate::sys::quick("dmesg", &[]));

    let Some(out) = out else { return Vec::new() };
    let Some(text) = out.text() else {
        // dmesg is commonly restricted to root by `kernel.dmesg_restrict`.
        return Vec::new();
    };

    const INTERESTING: [&str; 10] = [
        "firmware",
        "out of memory",
        "oom-kill",
        "i/o error",
        "ata error",
        "remount",
        "read-only",
        "segfault",
        "thermal",
        "hardware error",
    ];

    let mut hits: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| {
            let low = l.to_ascii_lowercase();
            INTERESTING.iter().any(|k| low.contains(k))
                || crate::probe::services::is_error_line(&low)
        })
        .map(|l| crate::sys::truncate_line(l, crate::probe::services::MAX_LOG_LINE))
        .collect();

    dedupe_keeping_order(&mut hits);
    let start = hits.len().saturating_sub(max_lines.min(20));
    hits.drain(..start);
    hits
}

fn journal_errors(max_lines: usize) -> Vec<String> {
    if !crate::sys::have("journalctl") {
        return Vec::new();
    }
    let n = max_lines.min(40).to_string();
    let out = crate::sys::run(
        "journalctl",
        &["-p", "err", "-b", "--no-pager", "-n", &n, "-o", "short"],
        std::time::Duration::from_secs(5),
    );
    let Some(out) = out else { return Vec::new() };
    let Some(text) = out.text() else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with("-- "))
        .map(|l| crate::sys::truncate_line(l, crate::probe::services::MAX_LOG_LINE))
        .collect();
    dedupe_keeping_order(&mut lines);
    lines
}

/// Collapse repeats while keeping first-seen order.
///
/// A service that retries every two seconds writes the same line hundreds of
/// times; showing it once is the whole of the information. The comparison
/// lives in `services` so that both log readers agree on what a repeat is.
fn dedupe_keeping_order(lines: &mut Vec<String>) {
    crate::probe::services::dedupe_similar(lines);
}

impl Logs {
    pub fn is_empty(&self) -> bool {
        self.raven.is_empty() && self.kernel.is_empty() && self.journal.is_empty()
    }

    pub fn total_errors(&self) -> usize {
        self.raven.iter().map(|r| r.errors.len()).sum::<usize>()
            + self.kernel.len()
            + self.journal.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_that_differ_only_in_numbers_collapse_to_one() {
        let mut lines = vec![
            "retry 1: connection refused".to_string(),
            "retry 2: connection refused".to_string(),
            "retry 3: connection refused".to_string(),
            "gave up".to_string(),
        ];
        dedupe_keeping_order(&mut lines);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "retry 1: connection refused");
        assert_eq!(lines[1], "gave up");
    }

    #[test]
    fn distinct_messages_all_survive() {
        let mut lines = vec![
            "cannot open /dev/foo".to_string(),
            "cannot open /dev/bar".to_string(),
        ];
        dedupe_keeping_order(&mut lines);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn an_empty_log_set_reports_itself_as_empty() {
        assert!(Logs::default().is_empty());
        assert_eq!(Logs::default().total_errors(), 0);
    }

    #[test]
    fn the_line_budget_is_clamped_to_something_sane() {
        // Nothing is asserted about content -- only that absurd inputs do not
        // make the probe read an unbounded amount.
        let l = probe(0);
        let _ = l.total_errors();
        let l = probe(usize::MAX);
        let _ = l.total_errors();
    }
}
