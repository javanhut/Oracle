//! Reading the machine.
//!
//! Two rules hold for everything in this file. First, it only reads: no probe
//! writes a file, starts a service, or changes a setting, and there is no code
//! path here that could. Second, every external command runs with a timeout
//! and a closed stdin, because a diagnostic tool that hangs on a wedged
//! subprocess has become the problem it was asked to look at.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The result of running a command that may not exist.
pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Whether the command was killed for running past its budget. This is
    /// kept separate from a non-zero exit because they mean opposite things: a
    /// command that failed told us something, and a command that timed out
    /// told us nothing at all.
    pub timed_out: bool,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == Some(0)
    }

    /// The first few meaningful lines of stderr, joined.
    ///
    /// One line is rarely enough. `pacman` opens with "failed to initialize
    /// alpm library:" and says *which* failure two lines later, so a probe
    /// that keeps only the first line throws away the entire diagnosis.
    pub fn error_message(&self) -> Option<String> {
        let joined: Vec<&str> = self
            .stderr
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .take(4)
            .collect();
        (!joined.is_empty()).then(|| joined.join(" "))
    }

    /// stdout when the command succeeded, otherwise nothing. Most callers want
    /// exactly this and have a sensible answer for "the tool is not installed".
    pub fn text(&self) -> Option<&str> {
        if self.ok() {
            Some(self.stdout.trim_end())
        } else {
            None
        }
    }
}

/// Run a command, giving up after `timeout`.
///
/// Returns `None` when the binary is not installed, which is a normal and
/// common answer on a minimal system rather than an error worth reporting.
///
/// stdout and stderr are drained on their own threads for the whole of the
/// child's life. Reading them after the wait instead looks simpler and is
/// wrong: a pipe holds about 64KB, and a command that writes more than that
/// blocks forever waiting for a reader that is itself waiting for the command
/// to exit. The symptom is a fast command appearing to hang until the timeout,
/// which a probe then reports as a broken system -- a false alarm produced
/// entirely by the tool that went looking.
pub fn run(bin: &str, args: &[&str], timeout: Duration) -> Option<Output> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A predictable locale keeps parsing honest: `df` and `ip` change their
        // wording under a translated locale, and a probe that half-parses is
        // worse than one that does not run.
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .spawn()
        .ok()?;

    let drain_out = child.stdout.take().map(spawn_drain);
    let drain_err = child.stderr.take().map(spawn_drain);

    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => break None,
        }
    };

    // The pipes are closed now, so both drains finish on their own.
    let stdout = drain_out
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr = drain_err
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();

    Some(Output {
        status: status.and_then(|s| s.code()),
        stdout,
        stderr,
        timed_out,
    })
}

/// Read a pipe to the end on its own thread.
fn spawn_drain<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        // Command output is usually UTF-8 and occasionally is not; a log line
        // with one bad byte should not cost us the other thousand.
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Run with the ordinary two-second budget. Every probe is expected to finish
/// well inside this; the timeout exists for the pathological case.
pub fn quick(bin: &str, args: &[&str]) -> Option<Output> {
    run(bin, args, Duration::from_secs(2))
}

/// Whether `bin` exists on `PATH` and is executable.
pub fn have(bin: &str) -> bool {
    which(bin).is_some()
}

pub fn which(bin: &str) -> Option<String> {
    if bin.contains('/') {
        return is_executable(Path::new(bin)).then(|| bin.to_string());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Read a small text file, ignoring anything that goes wrong. Used for
/// `/proc` and `/sys` entries, where a missing file simply means the kernel
/// does not have that to say on this machine.
pub fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

pub fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    read(path).map(|s| s.trim().to_string())
}

/// Read at most the last `n` lines of a file without loading all of it.
///
/// Log files grow without bound; a probe that reads a 400MB log to show the
/// last twenty lines is a bug even when it works.
pub fn tail(path: impl AsRef<Path>, n: usize) -> Option<Vec<String>> {
    use std::io::{Seek, SeekFrom};
    let path = path.as_ref();
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();

    // 512 bytes per line is generous for a log; cap the read either way.
    let window = ((n as u64 + 1) * 512).min(256 * 1024).min(len);
    f.seek(SeekFrom::Start(len - window)).ok()?;
    let mut buf = Vec::with_capacity(window as usize);
    f.read_to_end(&mut buf).ok()?;

    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    // The first line is probably cut in half by the seek.
    if window < len && !lines.is_empty() {
        lines.remove(0);
    }
    let start = lines.len().saturating_sub(n);
    Some(lines[start..].to_vec())
}

/// The `/proc/meminfo` value for `key`, in bytes.
pub fn meminfo(key: &str) -> Option<u64> {
    let text = read("/proc/meminfo")?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(key)
            && rest.starts_with(':')
        {
            let kb: u64 = rest[1..].split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Free and total bytes on the filesystem holding `path`.
///
/// This shells out to `df` rather than calling `statvfs`, which would mean a
/// libc dependency and an `unsafe` block. `df` is in the base system, the
/// parse is stable under `LC_ALL=C`, and a diagnostic tool can afford the
/// process.
pub fn disk_usage(path: &str) -> Option<(u64, u64)> {
    let out = quick("df", &["-Pk", "--", path])?;
    let text = out.text()?;
    let line = text.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    // Filesystem 1024-blocks Used Available Capacity Mounted-on
    if cols.len() < 4 {
        return None;
    }
    let total: u64 = cols[1].parse().ok()?;
    let avail: u64 = cols[3].parse().ok()?;
    Some((avail * 1024, total * 1024))
}

/// Uptime in seconds.
pub fn uptime() -> Option<u64> {
    let text = read("/proc/uptime")?;
    let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
    Some(secs as u64)
}

/// When this boot started, in seconds since the epoch.
pub fn boot_time() -> Option<u64> {
    read("/proc/stat")?
        .lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()
}

/// When a file was last written, in seconds since the epoch.
pub fn modified_epoch(path: impl AsRef<Path>) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// The first line of a file, without reading the rest of it.
pub fn first_line(path: impl AsRef<Path>) -> Option<String> {
    use std::io::{BufRead, Read};
    let f = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(f.take(4096))
        .read_line(&mut line)
        .ok()?;
    let line = line.trim_end_matches(['\n', '\r']).to_string();
    (!line.is_empty()).then_some(line)
}

/// Bound a log line to something that fits in a report.
///
/// Logs contain single lines thousands of characters long -- a compiler
/// invocation, a JSON blob, a stack trace on one line. Printing one whole
/// buries every other finding on the page, and sending one to a small model
/// spends most of its context on a command line nobody needs to read.
pub fn truncate_line(line: &str, max: usize) -> String {
    let line = line.trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let head: String = line.chars().take(max).collect();
    format!(
        "{head}… [{} characters truncated]",
        line.chars().count() - max
    )
}

/// A duration as something a person would say.
pub fn humanise_duration(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

/// Whether this process is running as root.
pub fn is_root() -> bool {
    // `id -u` avoids a libc dependency for the one number we need.
    quick("id", &["-u"])
        .and_then(|o| o.text().map(|t| t.trim() == "0"))
        .unwrap_or(false)
}

/// The groups this account belongs to.
pub fn groups() -> Vec<String> {
    quick("id", &["-Gn"])
        .and_then(|o| {
            o.text()
                .map(|t| t.split_whitespace().map(String::from).collect())
        })
        .unwrap_or_default()
}

/// Whether this looks like a Raven Linux system.
///
/// Oracle runs perfectly well on any Linux -- the Raven-specific probes simply
/// find nothing and stay quiet -- so this only decides how much Raven-flavoured
/// context is worth gathering.
pub fn is_raven() -> bool {
    Path::new("/etc/raven").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_binary_is_none_not_a_panic() {
        assert!(
            run(
                "definitely-not-a-real-binary-oracle",
                &[],
                Duration::from_secs(1)
            )
            .is_none()
        );
    }

    #[test]
    fn a_command_writing_more_than_a_pipe_holds_does_not_deadlock() {
        // A pipe buffers about 64KB. Draining only after the wait made any
        // command with more output than that hang until its timeout, which
        // probes then reported as a broken system.
        let out = run(
            "sh",
            &["-c", "head -c 2000000 /dev/zero | tr '\\0' 'x'"],
            Duration::from_secs(10),
        )
        .expect("sh exists");
        assert!(!out.timed_out, "draining must keep pace with the child");
        assert!(out.ok());
        assert_eq!(out.stdout.len(), 2_000_000);
    }

    #[test]
    fn a_timeout_is_distinguishable_from_a_failure() {
        let slow = run("sleep", &["30"], Duration::from_millis(200)).expect("sleep exists");
        assert!(slow.timed_out);
        assert!(!slow.ok());

        let failed = run("sh", &["-c", "exit 3"], Duration::from_secs(5)).expect("sh exists");
        assert!(!failed.timed_out, "a clean non-zero exit is not a timeout");
        assert_eq!(failed.status, Some(3));
    }

    #[test]
    fn an_error_message_keeps_the_line_that_names_the_cause() {
        let out = run(
            "sh",
            &["-c", "echo 'failed to initialize:' >&2; echo '(context)' >&2; echo 'wrong version' >&2; exit 1"],
            Duration::from_secs(5),
        )
        .expect("sh exists");
        let msg = out.error_message().expect("stderr had content");
        assert!(msg.contains("wrong version"), "got {msg}");
    }

    #[test]
    fn which_finds_something_that_exists() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-real-binary-oracle").is_none());
    }

    #[test]
    fn tail_returns_the_end_of_a_file() {
        let dir = std::env::temp_dir().join(format!("oracle-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("log");
        let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&p, body).unwrap();
        let lines = tail(&p, 3).unwrap();
        assert_eq!(lines, vec!["line 497", "line 498", "line 499"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_of_a_short_file_is_the_whole_file() {
        let dir = std::env::temp_dir().join(format!("oracle-tail2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("log");
        std::fs::write(&p, "a\nb\n").unwrap();
        assert_eq!(tail(&p, 10).unwrap(), vec!["a", "b"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn meminfo_reports_something_plausible() {
        let total = meminfo("MemTotal").expect("every Linux has MemTotal");
        assert!(total > 64 * 1024 * 1024);
    }

    #[test]
    fn a_long_line_is_cut_and_says_how_much_was_cut() {
        let long = "x".repeat(500);
        let out = truncate_line(&long, 100);
        assert!(out.starts_with(&"x".repeat(100)));
        assert!(out.contains("400 characters truncated"), "got {out}");
    }

    #[test]
    fn a_short_line_is_returned_whole() {
        assert_eq!(truncate_line("  already short  ", 80), "already short");
    }

    #[test]
    fn durations_read_like_english() {
        assert_eq!(humanise_duration(45), "45s");
        assert_eq!(humanise_duration(3 * 60), "3m");
        assert_eq!(humanise_duration(2 * 3600 + 5 * 60), "2h 5m");
        assert_eq!(humanise_duration(3 * 86400 + 3600), "3d 1h");
    }
}
