//! Read-only inspection of the machine.
//!
//! A probe gathers facts. It does not judge them -- that is `diagnose` -- and
//! it does not fix them, because nothing in Oracle fixes anything. Each probe
//! is gated by a switch in `[context]`, and a switch that is off means the
//! probe does not run at all, so its data cannot reach a model by any route.
//!
//! Probes are written to find nothing gracefully. Oracle runs on any Linux;
//! on a machine without `/etc/raven` the Raven-specific probes simply report
//! that there is nothing there, rather than treating absence as a fault.

pub mod hardware;
pub mod logs;
pub mod network;
pub mod packages;
pub mod services;
pub mod storage;

use crate::config::Config;
use serde::Serialize;

/// Everything Oracle knows about this machine at one moment.
///
/// A `None` field means the probe was switched off, which is different from a
/// probe that ran and found nothing. The distinction matters in the report:
/// Oracle should say "you told me not to look" rather than "all clear".
#[derive(Debug, Default, Serialize)]
pub struct SystemView {
    pub host: HostFacts,
    pub services: Option<services::Services>,
    pub storage: Option<storage::Storage>,
    pub network: Option<network::Network>,
    pub packages: Option<packages::Packages>,
    pub logs: Option<logs::Logs>,
    pub hardware: Option<hardware::Hardware>,
}

/// The handful of facts worth having whatever else is switched off.
#[derive(Debug, Default, Serialize)]
pub struct HostFacts {
    pub kernel: String,
    pub distro: String,
    pub is_raven: bool,
    pub init: String,
    pub uptime_seconds: u64,
    pub running_as_root: bool,
    pub desktop: Option<String>,
}

/// How the caller wants the probes run.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProbeOptions {
    /// Permit checks that touch the network: a DNS lookup, a gateway probe.
    /// Off by default -- a passive read of the routing table answers most
    /// questions, and a tool that quietly emits traffic is not a quiet tool.
    pub online_checks: bool,
    /// Permit checks that are slow or that spawn the package manager: the
    /// pending-update count, mostly.
    pub slow_checks: bool,
}

impl SystemView {
    /// Gather everything the config permits.
    pub fn gather(cfg: &Config, opts: ProbeOptions) -> SystemView {
        let c = &cfg.context;
        SystemView {
            host: host_facts(),
            services: c.services.then(services::probe),
            storage: c.storage.then(storage::probe),
            network: c.network.then(|| network::probe(opts)),
            packages: c.packages.then(|| packages::probe(opts)),
            logs: c.logs.then(|| logs::probe(c.log_lines)),
            hardware: c.hardware.then(hardware::probe),
        }
    }

    /// Gather only what a specific area needs.
    ///
    /// `oracle ask "why won't my wifi connect"` has no business reading the
    /// package database. Narrowing the gather keeps the prompt small, the
    /// answer focused, and the amount of the machine that gets described to a
    /// model no larger than the question requires.
    pub fn gather_for(cfg: &Config, areas: &[Area], opts: ProbeOptions) -> SystemView {
        let c = &cfg.context;
        let want = |a: Area| areas.is_empty() || areas.contains(&a);
        SystemView {
            host: host_facts(),
            services: (c.services && want(Area::Services)).then(services::probe),
            storage: (c.storage && want(Area::Storage)).then(storage::probe),
            network: (c.network && want(Area::Network)).then(|| network::probe(opts)),
            packages: (c.packages && want(Area::Packages)).then(|| packages::probe(opts)),
            logs: (c.logs && want(Area::Logs)).then(|| logs::probe(c.log_lines)),
            hardware: (c.hardware && want(Area::Hardware)).then(hardware::probe),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    Services,
    Storage,
    Network,
    Packages,
    Logs,
    Hardware,
}

impl Area {
    pub fn parse(s: &str) -> Option<Area> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "services" | "service" | "init" | "boot" => Area::Services,
            "storage" | "disk" | "disks" | "space" => Area::Storage,
            "network" | "net" | "wifi" | "wireless" => Area::Network,
            "packages" | "package" | "pkg" | "rvn" => Area::Packages,
            "logs" | "log" | "journal" => Area::Logs,
            "hardware" | "hw" | "cpu" | "memory" => Area::Hardware,
            _ => return None,
        })
    }

    pub fn all() -> [Area; 6] {
        [
            Area::Services,
            Area::Storage,
            Area::Network,
            Area::Packages,
            Area::Logs,
            Area::Hardware,
        ]
    }

    pub fn name(self) -> &'static str {
        match self {
            Area::Services => "services",
            Area::Storage => "storage",
            Area::Network => "network",
            Area::Packages => "packages",
            Area::Logs => "logs",
            Area::Hardware => "hardware",
        }
    }
}

/// Guess which areas a free-text question is about.
///
/// This only ever narrows what gets read. When nothing matches, the caller
/// gathers everything, so a bad guess costs a slightly larger prompt rather
/// than a wrong answer.
pub fn areas_for_question(question: &str) -> Vec<Area> {
    let q = question.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut add = |a: Area| {
        if !out.contains(&a) {
            out.push(a);
        }
    };

    const NETWORK: [&str; 16] = [
        "wifi", "wi-fi", "wireless", "network", "internet", "ethernet", "dns", "ip ", "router",
        "offline", "online", "connect", "ssid", "caw", "vpn", "ping",
    ];
    const STORAGE: [&str; 10] = [
        "disk",
        "space",
        "full",
        "storage",
        "partition",
        "mount",
        "filesystem",
        "df",
        "inode",
        "no space",
    ];
    const PACKAGES: [&str; 12] = [
        "install",
        "package",
        "rvn",
        "pacman",
        "aur",
        "update",
        "upgrade",
        "repo",
        "mirror",
        "dependency",
        "uninstall",
        "store",
    ];
    const SERVICES: [&str; 11] = [
        "service",
        "daemon",
        "boot",
        "start",
        "startup",
        "init",
        "failed",
        "crash",
        "restart",
        "systemd",
        "raven-init",
    ];
    const LOGS: [&str; 6] = ["log", "journal", "error", "dmesg", "message", "trace"];
    const HARDWARE: [&str; 13] = [
        "cpu",
        "memory",
        "ram",
        "slow",
        "hot",
        "temperature",
        "fan",
        "battery",
        "gpu",
        "graphics",
        "audio",
        "sound",
        "bluetooth",
    ];

    for (words, area) in [
        (&NETWORK[..], Area::Network),
        (&STORAGE[..], Area::Storage),
        (&PACKAGES[..], Area::Packages),
        (&SERVICES[..], Area::Services),
        (&LOGS[..], Area::Logs),
        (&HARDWARE[..], Area::Hardware),
    ] {
        if words.iter().any(|w| q.contains(w)) {
            add(area);
        }
    }

    // Logs are corroboration for almost anything, so bring them along whenever
    // the question landed on some other area.
    if !out.is_empty() && !out.contains(&Area::Logs) {
        out.push(Area::Logs);
    }
    out
}

fn host_facts() -> HostFacts {
    HostFacts {
        kernel: crate::sys::read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_default(),
        distro: distro_name(),
        is_raven: crate::sys::is_raven(),
        init: init_system(),
        uptime_seconds: crate::sys::uptime().unwrap_or(0),
        running_as_root: crate::sys::is_root(),
        desktop: std::env::var("XDG_CURRENT_DESKTOP")
            .ok()
            .filter(|s| !s.is_empty()),
    }
}

fn distro_name() -> String {
    for path in ["/etc/os-release", "/usr/lib/os-release"] {
        if let Some(text) = crate::sys::read(path) {
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
                    return v.trim().trim_matches('"').to_string();
                }
            }
        }
    }
    if crate::sys::is_raven() {
        return "Raven Linux".into();
    }
    "unknown".into()
}

/// Which init is actually running, read from PID 1 rather than from what is
/// installed. A box can have both raven-init and systemd on disk; only one of
/// them booted it, and that is the one whose service files matter.
pub fn init_system() -> String {
    let comm = crate::sys::read_trimmed("/proc/1/comm").unwrap_or_default();

    // "init" is the traditional name and tells us nothing -- systemd, raven-init
    // and busybox all answer to it. The symlink at /proc/1/exe names the
    // binary that is actually running, when this account may read it.
    if comm.is_empty() || comm == "init" {
        if let Ok(exe) = std::fs::read_link("/proc/1/exe")
            && let Some(name) = exe.file_name()
        {
            let name = name.to_string_lossy();
            if !name.is_empty() && name != "init" {
                return name.into_owned();
            }
        }
        // The first word of the command line is the next best thing, and
        // following it through any symlinks is better still: `/sbin/init` is a
        // link to `raven-init` here and to `systemd` on an Arch box, which is
        // the whole of what we wanted to know. Both reads work as an ordinary
        // user, unlike /proc/1/exe.
        if let Some(cmdline) = crate::sys::read("/proc/1/cmdline")
            && let Some(first) = cmdline.split('\0').next()
            && !first.is_empty()
        {
            if let Ok(resolved) = std::fs::canonicalize(first)
                && let Some(name) = resolved.file_name()
            {
                let name = name.to_string_lossy();
                if !name.is_empty() && name != "init" {
                    return name.into_owned();
                }
            }
            let base = first.rsplit('/').next().unwrap_or(first);
            if base != "init" {
                return base.to_string();
            }
        }
    }

    if comm.is_empty() {
        "unknown".into()
    } else {
        comm
    }
}

/// How serious a finding is.
///
/// The ordering is the report order, and `Note` is the floor for anything
/// Oracle volunteers. There is no severity for "you could tidy this up": a
/// troubleshooting tool that lists opinions alongside faults teaches people to
/// stop reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Something is broken now and the user has almost certainly noticed.
    Critical,
    /// Something is broken or about to break, and the user may not have
    /// noticed. Silent failures live here.
    Warning,
    /// Worth knowing, not worth acting on today.
    Note,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }

    pub fn paint(self, s: &str) -> String {
        match self {
            Severity::Critical => crate::ui::red(s),
            Severity::Warning => crate::ui::yellow(s),
            Severity::Note => crate::ui::blue(s),
        }
    }
}

/// Something Oracle noticed, with what it saw and what it would try.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// A stable identifier, so a finding can be referred to across runs and
    /// suppressed by name.
    pub id: String,
    pub severity: Severity,
    pub title: String,
    /// What Oracle actually read. Findings without evidence are opinions.
    pub evidence: Vec<String>,
    pub suggestions: Vec<Suggestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Suggestion {
    pub what: String,
    /// A command the user may choose to run. Oracle prints it. Oracle does not
    /// run it.
    pub command: Option<String>,
    pub needs_root: bool,
}

impl Suggestion {
    pub fn new(what: impl Into<String>) -> Suggestion {
        Suggestion {
            what: what.into(),
            command: None,
            needs_root: false,
        }
    }

    pub fn cmd(what: impl Into<String>, command: impl Into<String>) -> Suggestion {
        Suggestion {
            what: what.into(),
            command: Some(command.into()),
            needs_root: false,
        }
    }

    pub fn root_cmd(what: impl Into<String>, command: impl Into<String>) -> Suggestion {
        Suggestion {
            what: what.into(),
            command: Some(command.into()),
            needs_root: true,
        }
    }
}

impl Finding {
    pub fn new(id: &str, severity: Severity, title: impl Into<String>) -> Finding {
        Finding {
            id: id.to_string(),
            severity,
            title: title.into(),
            evidence: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    pub fn evidence(mut self, e: impl Into<String>) -> Finding {
        self.evidence.push(e.into());
        self
    }

    pub fn suggest(mut self, s: Suggestion) -> Finding {
        self.suggestions.push(s);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wifi_question_does_not_pull_in_the_package_database() {
        let areas = areas_for_question("why won't my wifi connect after suspend");
        assert!(areas.contains(&Area::Network));
        assert!(!areas.contains(&Area::Packages));
    }

    #[test]
    fn an_install_question_reaches_packages_and_logs() {
        let areas = areas_for_question("rvn install keeps failing");
        assert!(areas.contains(&Area::Packages));
        assert!(areas.contains(&Area::Logs), "logs corroborate every area");
    }

    #[test]
    fn an_unclassifiable_question_narrows_nothing() {
        assert!(areas_for_question("what is going on").is_empty());
    }

    #[test]
    fn the_init_system_is_named_rather_than_called_init() {
        // "init" is what every init calls itself and tells a diagnosis
        // nothing. Following /sbin/init through its symlink names the real one.
        let name = init_system();
        assert!(!name.is_empty());
        assert_ne!(
            name, "init",
            "the generic name means the symlink was not followed"
        );
    }

    #[test]
    fn severity_sorts_worst_first() {
        let mut v = vec![Severity::Note, Severity::Critical, Severity::Warning];
        v.sort();
        assert_eq!(
            v,
            vec![Severity::Critical, Severity::Warning, Severity::Note]
        );
    }
}
