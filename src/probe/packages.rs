//! Packages: whether installing things can work at all.
//!
//! "It won't install" is rarely about the package. It is a stale database
//! lock, a `rvnd` socket that never appeared, an account outside `wheel`, or a
//! local database written by a newer libalpm than the one now installed. Each
//! of those produces a different error message and none of them says what is
//! actually wrong.

use crate::probe::ProbeOptions;
use serde::Serialize;
use std::path::Path;

/// One package tool's attempt to read the local database.
#[derive(Debug, Clone, Serialize)]
pub struct DbReader {
    pub tool: String,
    pub ok: bool,
    pub error: Option<String>,
    /// The check did not finish, so it proved nothing either way.
    pub inconclusive: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Packages {
    /// `rvn` on Raven, `pacman` elsewhere, or neither.
    pub manager: Option<String>,
    pub rvn_present: bool,
    pub pacman_present: bool,
    /// Whether the `rvnd` control socket exists, which is what lets a member
    /// of `wheel` install without a password.
    pub rvnd_socket: Option<bool>,
    pub in_wheel: bool,
    /// A lock file left behind by an interrupted transaction.
    pub stale_lock: Option<String>,
    /// The declared on-disk format of the local database.
    pub local_db_version: Option<String>,
    /// What happened when each installed package tool was asked to read it.
    ///
    /// This is a list rather than one verdict because the tools disagree in
    /// practice: `rvn` implements libalpm itself and `pacman` links the system
    /// one, so an interrupted upgrade can leave a database that one reads
    /// happily and the other refuses. Collapsing that into a single boolean
    /// would report either a disaster or an all-clear, and neither is true.
    pub db_readers: Vec<DbReader>,
    /// Age of the last repository sync.
    pub sync_age_seconds: Option<u64>,
    /// Only filled in when slow checks were permitted.
    pub pending_updates: Option<usize>,
    pub installed_count: Option<usize>,
}

const LOCAL_DB: &str = "/var/lib/pacman/local";
const SYNC_DB: &str = "/var/lib/pacman/sync";
const LOCK: &str = "/var/lib/pacman/db.lck";
const RVND_SOCKET: &str = "/run/rvn/ctl";

pub fn probe(opts: ProbeOptions) -> Packages {
    let mut p = Packages {
        rvn_present: crate::sys::have("rvn"),
        pacman_present: crate::sys::have("pacman"),
        in_wheel: crate::sys::groups().iter().any(|g| g == "wheel"),
        ..Default::default()
    };

    p.manager = if p.rvn_present {
        Some("rvn".into())
    } else if p.pacman_present {
        Some("pacman".into())
    } else {
        None
    };

    if crate::sys::have("rvnd") || Path::new(RVND_SOCKET).exists() {
        p.rvnd_socket = Some(Path::new(RVND_SOCKET).exists());
    }

    if Path::new(LOCK).exists() {
        p.stale_lock = Some(LOCK.into());
    }

    read_local_db(&mut p);
    p.sync_age_seconds = newest_mtime_age(SYNC_DB);

    if opts.slow_checks {
        p.pending_updates = count_pending();
    }

    p
}

/// Read the local database version, and ask each installed tool to open it.
///
/// The cheapest honest test of readability is to make a tool read it. Both
/// commands below are small, local and read-only: `pacman -Qq` prints names
/// and `rvn list` prints a short table. Neither touches the network, and
/// neither can change anything.
fn read_local_db(p: &mut Packages) {
    if !Path::new(LOCAL_DB).is_dir() {
        return;
    }

    if let Some(v) = crate::sys::read_trimmed(format!("{LOCAL_DB}/ALPM_DB_VERSION")) {
        p.local_db_version = Some(v);
    }

    p.installed_count = std::fs::read_dir(LOCAL_DB)
        .ok()
        .map(|d| d.flatten().filter(|e| e.path().is_dir()).count());

    let checks: [(&str, bool, &[&str]); 2] = [
        ("rvn", p.rvn_present, &["list"]),
        ("pacman", p.pacman_present, &["-Qq"]),
    ];

    for (tool, present, args) in checks {
        if !present {
            continue;
        }
        let Some(out) = crate::sys::run(tool, args, std::time::Duration::from_secs(10)) else {
            continue;
        };
        p.db_readers.push(DbReader {
            tool: tool.to_string(),
            ok: out.ok(),
            // A timeout is reported as inconclusive rather than as a failure:
            // it means the check did not finish, which is a fact about the
            // check and not about the database.
            error: (!out.ok() && !out.timed_out)
                .then(|| out.error_message())
                .flatten(),
            inconclusive: out.timed_out,
        });
    }
}

/// How long ago the newest file in a directory was modified.
fn newest_mtime_age(dir: &str) -> Option<u64> {
    let entries = std::fs::read_dir(dir).ok()?;
    let newest = entries
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter_map(|m| m.modified().ok())
        .max()?;
    newest.elapsed().ok().map(|d| d.as_secs())
}

/// Count pending updates without touching the network.
///
/// `--no-refresh` is the important flag: Oracle reports on the databases the
/// machine already has and never syncs them, because syncing is a change to
/// the system and Oracle does not change the system.
fn count_pending() -> Option<usize> {
    if !crate::sys::have("rvn") {
        return None;
    }
    let out = crate::sys::run(
        "rvn",
        &["update", "--dry-run", "--no-refresh"],
        std::time::Duration::from_secs(20),
    )?;
    let text = out.text()?;
    // Count lines that name a version transition; the exact wording of the
    // summary line has changed before and may change again.
    let n = text
        .lines()
        .filter(|l| l.contains("->") || l.contains("=>"))
        .count();
    Some(n)
}

impl Packages {
    /// Tools that tried to read the database and could not.
    pub fn db_failures(&self) -> Vec<&DbReader> {
        self.db_readers
            .iter()
            .filter(|r| !r.ok && !r.inconclusive)
            .collect()
    }

    /// Tools that read it successfully.
    pub fn db_successes(&self) -> Vec<&DbReader> {
        self.db_readers.iter().filter(|r| r.ok).collect()
    }

    /// True when every tool that gave an answer gave a bad one.
    pub fn db_wholly_unreadable(&self) -> bool {
        !self.db_failures().is_empty() && self.db_successes().is_empty()
    }

    /// Whether an unprivileged install can work right now.
    pub fn can_install_without_password(&self) -> bool {
        self.rvnd_socket == Some(true) && self.in_wheel
    }

    pub fn sync_is_stale(&self, days: u64) -> bool {
        self.sync_age_seconds
            .map(|s| s > days * 86_400)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(tool: &str, ok: bool, err: Option<&str>) -> DbReader {
        DbReader {
            tool: tool.into(),
            ok,
            error: err.map(String::from),
            inconclusive: false,
        }
    }

    #[test]
    fn one_tool_failing_while_another_succeeds_is_not_a_dead_database() {
        let p = Packages {
            db_readers: vec![
                reader("rvn", true, None),
                reader("pacman", false, Some("database is incorrect version")),
            ],
            ..Default::default()
        };
        assert!(!p.db_wholly_unreadable());
        assert_eq!(p.db_failures().len(), 1);
        assert_eq!(p.db_successes().len(), 1);
    }

    #[test]
    fn every_tool_failing_is_a_dead_database() {
        let p = Packages {
            db_readers: vec![
                reader("rvn", false, Some("x")),
                reader("pacman", false, Some("y")),
            ],
            ..Default::default()
        };
        assert!(p.db_wholly_unreadable());
    }

    #[test]
    fn an_inconclusive_check_is_never_treated_as_a_failure() {
        // A check that timed out proves nothing. Counting it as a failure is
        // how a slow machine gets told its package database is broken.
        let p = Packages {
            db_readers: vec![DbReader {
                tool: "rvn".into(),
                ok: false,
                error: None,
                inconclusive: true,
            }],
            ..Default::default()
        };
        assert!(p.db_failures().is_empty());
        assert!(!p.db_wholly_unreadable());
    }

    #[test]
    fn the_real_machine_gets_a_verdict_from_every_installed_tool() {
        let p = probe(ProbeOptions::default());
        if p.rvn_present || p.pacman_present {
            assert!(
                !p.db_readers.is_empty(),
                "an installed package manager must be asked"
            );
        }
    }

    #[test]
    fn installing_without_a_password_needs_both_the_socket_and_the_group() {
        let mut p = Packages {
            rvnd_socket: Some(true),
            in_wheel: true,
            ..Default::default()
        };
        assert!(p.can_install_without_password());

        p.in_wheel = false;
        assert!(!p.can_install_without_password());

        p.in_wheel = true;
        p.rvnd_socket = Some(false);
        assert!(!p.can_install_without_password());

        p.rvnd_socket = None;
        assert!(!p.can_install_without_password());
    }

    #[test]
    fn staleness_is_measured_in_days() {
        let p = Packages {
            sync_age_seconds: Some(10 * 86_400),
            ..Default::default()
        };
        assert!(p.sync_is_stale(7));
        assert!(!p.sync_is_stale(30));
    }

    #[test]
    fn an_unknown_sync_age_is_not_stale() {
        let p = Packages::default();
        assert!(!p.sync_is_stale(1));
    }

    #[test]
    fn a_passive_probe_does_not_count_pending_updates() {
        let p = probe(ProbeOptions::default());
        assert_eq!(p.pending_updates, None);
    }
}
