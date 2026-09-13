//! Turning facts into findings.
//!
//! Every rule in this file runs without a model. That is the point: on a
//! machine with no inference server, no downloaded weights and no network,
//! `oracle doctor` still tells you that a service has been failing silently
//! since install, that the root filesystem is at 98%, or that the firmware
//! your wifi card wants was never installed. The model's job is to explain and
//! to answer open questions, never to be the only thing that noticed.
//!
//! A rule earns its place by meeting three tests. It has to be specific enough
//! that a human reading the evidence agrees; it has to describe a situation
//! somebody can act on; and it has to stay quiet on a healthy machine. A rule
//! that fires on a working system trains people to ignore the report, which
//! costs more than the rule was ever worth.

use crate::probe::{Finding, Severity, Suggestion, SystemView};

/// Thresholds, gathered here so the judgement calls are visible in one place
/// rather than scattered through the rules.
mod threshold {
    /// A filesystem this full is a problem now.
    pub const DISK_CRITICAL_PCT: u8 = 95;
    /// A filesystem this full will be a problem soon.
    pub const DISK_WARNING_PCT: u8 = 85;
    /// Inodes are invisible until they run out, so warn earlier.
    pub const INODE_WARNING_PCT: u8 = 90;
    /// Memory pressure worth mentioning, measured against MemAvailable.
    pub const MEMORY_WARNING_PCT: u8 = 92;
    /// Swap this full on a desktop means the machine is about to stall.
    pub const SWAP_WARNING_PCT: u8 = 60;
    /// Runnable work per core before "slow" has a measurable cause.
    pub const LOAD_PER_CORE: f32 = 2.5;
    /// Sustained temperature at which throttling is likely.
    pub const THERMAL_WARNING_C: f32 = 90.0;
    /// Repository databases older than this make install errors confusing.
    pub const SYNC_STALE_DAYS: u64 = 21;
    /// Battery health below which capacity loss explains short runtimes.
    pub const BATTERY_HEALTH_PCT: u8 = 70;
    /// How recently a log must have been written for its errors to count as
    /// happening rather than as history.
    pub const LOG_FRESH_SECONDS: u64 = 7 * 86_400;
    /// How many complaining services to name before the list stops being
    /// information and starts being noise.
    pub const MAX_NOISY_SERVICES: usize = 3;
}

/// Run every rule against what was gathered, worst first.
pub fn run(view: &SystemView) -> Vec<Finding> {
    let mut findings = Vec::new();

    services(view, &mut findings);
    storage(view, &mut findings);
    network(view, &mut findings);
    packages(view, &mut findings);
    hardware(view, &mut findings);
    logs(view, &mut findings);

    // Worst first, then alphabetically by id so two runs on an unchanged
    // machine produce byte-identical reports. A report that reshuffles itself
    // cannot be diffed, and diffing two reports is how people work out what
    // changed after a reboot.
    findings.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.id.cmp(&b.id)));
    findings
}

fn services(view: &SystemView, out: &mut Vec<Finding>) {
    let Some(s) = &view.services else { return };

    if let Some(err) = &s.error {
        out.push(
            Finding::new(
                "services.unparseable",
                Severity::Critical,
                "A service definition does not parse",
            )
            .evidence(err.clone())
            .suggest(Suggestion::new(
                "raven-init refuses to start a service whose file is invalid, and says so only \
                 in its own log. Fix the syntax and the service comes back on the next boot.",
            )),
        );
    }

    // The headline rule. An enabled service whose executable does not exist
    // fails on every boot; with critical = false it fails without telling
    // anyone, which is how a configured service can be dead for months.
    for svc in s.silent_failures() {
        let quietly = if svc.critical {
            "and the boot stops when it does"
        } else {
            "and because it is not critical, nothing reports the failure"
        };
        out.push(
            Finding::new(
                "services.exec-missing",
                if svc.critical {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                format!("{} is enabled but its program is not installed", svc.name),
            )
            .evidence(format!("{} declares exec = {}", svc.source, svc.exec))
            .evidence(format!("{} does not exist on this system", svc.exec))
            .evidence(format!("This service fails at every boot {quietly}"))
            .suggest(Suggestion::new(format!(
                "Either install the program {} expects, or disable the service by setting \
                 enabled = false in {}",
                svc.name, svc.source
            ))),
        );
    }

    for svc in s.not_ready() {
        let Some(path) = &svc.ready_path else {
            continue;
        };
        out.push(
            Finding::new(
                "services.not-ready",
                Severity::Warning,
                format!("{} is enabled but has not come up", svc.name),
            )
            .evidence(format!(
                "{} promises to create {path}, which is not there",
                svc.name
            ))
            .evidence(format!(
                "Anything that waits for {} will fall back or fail",
                svc.name
            ))
            .suggest(Suggestion::cmd(
                format!("Look at what {} logged", svc.name),
                svc.log_path
                    .clone()
                    .map(|p| format!("tail -n 40 {p}"))
                    .unwrap_or_else(|| format!("raven-rc status {}", svc.name)),
            )),
        );
    }

    for (svc, why) in s.broken_dependencies() {
        out.push(
            Finding::new(
                "services.dependency",
                Severity::Warning,
                format!("{} depends on a service that will not start", svc.name),
            )
            .evidence(format!("{} lists after = {:?}", svc.name, svc.after))
            .evidence(why)
            .suggest(Suggestion::new(
                "raven-init treats `after` as a dependency it must start, so a missing or \
                 disabled target can hold back everything behind it.",
            )),
        );
    }

    // Services that are running now and complaining now. The freshness window
    // is what keeps this from resurfacing a bad week from last month, and the
    // cap is what keeps a noisy machine from burying the findings above.
    for svc in s
        .logging_errors(threshold::LOG_FRESH_SECONDS)
        .into_iter()
        .take(threshold::MAX_NOISY_SERVICES)
    {
        let mut f = Finding::new(
            "services.logging-errors",
            Severity::Note,
            format!("{} is logging errors", svc.name),
        );
        for line in svc.log_errors.iter().take(3) {
            f = f.evidence(line.clone());
        }
        if let Some(path) = &svc.log_path {
            f = f.suggest(Suggestion::cmd(
                "Read the full log",
                format!("tail -n 100 {path}"),
            ));
        }
        out.push(f);
    }

    for unit in &s.systemd_failed {
        out.push(
            Finding::new(
                "services.systemd-failed",
                Severity::Warning,
                format!("{} has failed", unit.unit),
            )
            .evidence(format!("systemctl reports {} as {}", unit.unit, unit.state))
            .suggest(Suggestion::cmd(
                "See why it stopped",
                format!("systemctl status {} --no-pager -l", unit.unit),
            ))
            .suggest(Suggestion::cmd(
                "Read its log for this boot",
                format!("journalctl -u {} -b --no-pager", unit.unit),
            )),
        );
    }
}

fn storage(view: &SystemView, out: &mut Vec<Finding>) {
    let Some(st) = &view.storage else { return };

    for fs in st.tight(threshold::DISK_WARNING_PCT) {
        if fs.used_percent >= threshold::DISK_CRITICAL_PCT {
            let mut f = Finding::new(
                "storage.full",
                Severity::Critical,
                format!("{} is {}% full", fs.mount, fs.used_percent),
            )
            .evidence(format!(
                "{} used of {}, leaving {} free on {} ({})",
                crate::ui::bytes(fs.used_bytes()),
                crate::ui::bytes(fs.total_bytes),
                crate::ui::bytes(fs.available_bytes),
                fs.mount,
                fs.device
            ))
            .evidence(
                "At this level installs fail, logs stop, and applications cannot save settings. \
                 The errors will name anything except the disk."
                    .to_string(),
            );

            // Point at the space this machine actually has to reclaim, rather
            // than at generic advice.
            if fs.mount == "/" {
                if let (Some(cache), Some(bytes)) = (&st.package_cache, st.package_cache_bytes)
                    && bytes > 512 * 1024 * 1024
                {
                    f = f
                        .evidence(format!(
                            "The package cache at {cache} holds {}",
                            crate::ui::bytes(bytes)
                        ))
                        .suggest(Suggestion::root_cmd(
                            "Clear cached package downloads, which are re-downloadable",
                            "rvn clean",
                        ));
                }
                if let (Some(dir), Some(bytes)) = (&st.log_dir, st.log_dir_bytes)
                    && bytes > 256 * 1024 * 1024
                {
                    f = f.evidence(format!("Logs under {dir} hold {}", crate::ui::bytes(bytes)));
                }
            }

            f = f.suggest(Suggestion::cmd(
                "Find the largest directories under the full filesystem",
                format!(
                    "du -xh --max-depth=2 {} 2>/dev/null | sort -rh | head -20",
                    fs.mount
                ),
            ));
            out.push(f);
        } else if fs.used_percent >= threshold::DISK_WARNING_PCT {
            out.push(
                Finding::new(
                    "storage.filling",
                    Severity::Warning,
                    format!("{} is {}% full", fs.mount, fs.used_percent),
                )
                .evidence(format!(
                    "{} free of {}",
                    crate::ui::bytes(fs.available_bytes),
                    crate::ui::bytes(fs.total_bytes)
                ))
                .suggest(Suggestion::cmd(
                    "See where the space went",
                    format!(
                        "du -xh --max-depth=2 {} 2>/dev/null | sort -rh | head -20",
                        fs.mount
                    ),
                )),
            );
        }
    }

    for fs in &st.filesystems {
        if let Some(inodes) = fs.inodes_used_percent
            && inodes >= threshold::INODE_WARNING_PCT
        {
            out.push(
                Finding::new(
                    "storage.inodes",
                    Severity::Warning,
                    format!("{} has used {inodes}% of its inodes", fs.mount),
                )
                .evidence(format!(
                    "{} is only {}% full by size, so this will look like a full disk with space \
                     apparently free",
                    fs.mount, fs.used_percent
                ))
                .suggest(Suggestion::cmd(
                    "Find the directories holding the most files",
                    format!(
                        "find {} -xdev -type d -printf '%h\\n' 2>/dev/null | sort | uniq -c | \
                         sort -rn | head -20",
                        fs.mount
                    ),
                )),
            );
        }
    }

    for fs in st.read_only() {
        {
            out.push(
                Finding::new(
                    "storage.read-only",
                    Severity::Critical,
                    format!("{} is mounted read-only", fs.mount),
                )
                .evidence(format!("{} on {} has the ro option", fs.device, fs.mount))
                .evidence(
                    "The kernel remounts a filesystem read-only when it hits an I/O or \
                     consistency error, so this is usually a symptom rather than a setting."
                        .to_string(),
                )
                .suggest(Suggestion::cmd(
                    "Look for the error that caused it",
                    "dmesg | grep -iE 'i/o error|remount|ext4|btrfs|xfs' | tail -20",
                )),
            );
        }
    }
}

fn network(view: &SystemView, out: &mut Vec<Finding>) {
    let Some(net) = &view.network else { return };

    // Nothing at all is a different problem from having a link but no address.
    if net.usable().is_empty() && !net.interfaces.is_empty() {
        let mut f = Finding::new(
            "network.no-link",
            Severity::Warning,
            "No network interface is up",
        );
        for i in net.down().iter().take(4) {
            f = f.evidence(format!(
                "{} is {} with {} carrier",
                i.name,
                i.operstate,
                if i.carrier { "a" } else { "no" }
            ));
        }
        if net.has_wireless() && crate::sys::have("caw") {
            f = f.suggest(Suggestion::cmd("List visible networks", "caw scan"));
            f = f.suggest(Suggestion::cmd("Connect to one", "caw connect <ssid>"));
        }
        out.push(f);
    } else if net.default_route.is_none() && !net.usable().is_empty() {
        out.push(
            Finding::new(
                "network.no-route",
                Severity::Warning,
                "An interface is up but there is no default route",
            )
            .evidence(format!(
                "Up: {}",
                net.usable()
                    .iter()
                    .map(|i| i.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .evidence(
                "Local addresses will work and everything beyond this network will not."
                    .to_string(),
            )
            .suggest(Suggestion::cmd("Show the routing table", "ip route")),
        );
    }

    for i in net.up_without_address() {
        out.push(
            Finding::new(
                "network.no-address",
                Severity::Warning,
                format!("{} is up but has no address", i.name),
            )
            .evidence(format!(
                "{} is {} with carrier, and holds no routable address",
                i.name, i.operstate
            ))
            .evidence("This is what a DHCP failure looks like from the desktop.".to_string())
            .suggest(Suggestion::cmd(
                "Check the addresses the kernel has",
                format!("ip addr show {}", i.name),
            )),
        );
    }

    if net.nameservers.is_empty() && net.default_route.is_some() {
        out.push(
            Finding::new(
                "network.no-dns",
                Severity::Warning,
                "There is a route out but no nameserver configured",
            )
            .evidence("/etc/resolv.conf lists no nameserver".to_string())
            .evidence(
                "Addresses will work and names will not, which reads as \"some things work and \
                 some do not\"."
                    .to_string(),
            ),
        );
    }

    if net.dns_resolves == Some(false) && net.default_route.is_some() {
        out.push(
            Finding::new(
                "network.dns-failing",
                Severity::Warning,
                "Name resolution is failing",
            )
            .evidence(format!(
                "A lookup failed with nameservers: {}",
                net.nameservers.join(", ")
            ))
            .evidence(
                "The route is up, so this is the resolver rather than the connection.".to_string(),
            ),
        );
    }

    if net.cawd_running == Some(false) && net.has_wireless() {
        out.push(
            Finding::new(
                "network.cawd-down",
                Severity::Note,
                "This machine has wireless hardware and cawd is not running",
            )
            .evidence(
                "cawd holds the EAPOL socket, so without it a connection drops at the access \
                 point's next group-key rotation, typically within the hour."
                    .to_string(),
            ),
        );
    }
}

fn packages(view: &SystemView, out: &mut Vec<Finding>) {
    let Some(p) = &view.packages else { return };

    // Every tool refuses the database: nothing can install until it is fixed.
    if p.db_wholly_unreadable() {
        let mut f = Finding::new(
            "packages.db-unreadable",
            Severity::Critical,
            "The package database cannot be read by anything",
        );
        for r in p.db_failures() {
            f = f.evidence(format!(
                "{} failed: {}",
                r.tool,
                r.error.as_deref().unwrap_or("no message")
            ));
        }
        f = f.evidence(
            "Until this is resolved nothing can be installed, removed or updated, and every \
             package command will fail with its own unrelated-sounding message."
                .to_string(),
        );
        if let Some(v) = &p.local_db_version {
            f = f.evidence(format!("The database declares format version {v}"));
        }
        for s in db_repair_suggestions(&p.db_failures()) {
            f = f.suggest(s);
        }
        out.push(f);
    }
    // One tool refuses and another does not. This is the confusing case: most
    // things work, and the ones that shell out to the broken tool fail with
    // errors that have nothing to do with each other.
    else if !p.db_failures().is_empty() {
        let failing: Vec<&str> = p.db_failures().iter().map(|r| r.tool.as_str()).collect();
        let working: Vec<&str> = p.db_successes().iter().map(|r| r.tool.as_str()).collect();

        let mut f = Finding::new(
            "packages.db-partial",
            Severity::Warning,
            format!(
                "{} cannot read the package database, though {} can",
                failing.join(" and "),
                working.join(" and ")
            ),
        );
        for r in p.db_failures() {
            f = f.evidence(format!(
                "{} failed: {}",
                r.tool,
                r.error.as_deref().unwrap_or("no message")
            ));
        }
        f = f.evidence(
            "Ordinary installs will work, and anything that shells out to the broken tool will \
             not. AUR builds run makepkg, which calls pacman, so they are the usual casualty."
                .to_string(),
        );
        if let Some(v) = &p.local_db_version {
            f = f.evidence(format!("The database declares format version {v}"));
        }
        for s in db_repair_suggestions(&p.db_failures()) {
            f = f.suggest(s);
        }
        out.push(f);
    }

    if let Some(lock) = &p.stale_lock {
        out.push(
            Finding::new(
                "packages.stale-lock",
                Severity::Warning,
                "A package database lock is present",
            )
            .evidence(format!("{lock} exists"))
            .evidence(
                "If no package operation is running, this is left over from one that was \
                 interrupted, and every new operation will refuse to start."
                    .to_string(),
            )
            .suggest(Suggestion::cmd(
                "Confirm nothing is actually running first",
                "pgrep -a 'rvn|pacman|makepkg'",
            ))
            .suggest(Suggestion::root_cmd(
                "Then, and only then, remove the lock",
                format!("rm {lock}"),
            )),
        );
    }

    if p.rvnd_socket == Some(false) {
        out.push(
            Finding::new(
                "packages.rvnd-down",
                Severity::Note,
                "rvnd is not running, so installs will need root",
            )
            .evidence("/run/rvn/ctl does not exist".to_string())
            .evidence(
                "The Store and an unprivileged `rvn install` both go through this socket; \
                 without it they have no way to make a change."
                    .to_string(),
            )
            .suggest(Suggestion::cmd("Check the daemon", "raven-rc status rvnd")),
        );
    } else if p.rvnd_socket == Some(true) && !p.can_install_without_password() {
        out.push(
            Finding::new(
                "packages.not-in-wheel",
                Severity::Note,
                "This account cannot install packages without root",
            )
            .evidence("rvnd is running, and this account is not in the wheel group".to_string())
            .evidence(
                "Membership of wheel is the whole of the credential for rvnd's socket.".to_string(),
            ),
        );
    }

    if p.sync_is_stale(threshold::SYNC_STALE_DAYS) {
        let days = p.sync_age_seconds.unwrap_or(0) / 86_400;
        out.push(
            Finding::new(
                "packages.stale-sync",
                Severity::Note,
                format!("The repository databases are {days} days old"),
            )
            .evidence(
                "Installing against stale databases gives 404s on packages that have since been \
                 rebuilt, which reads as a broken mirror."
                    .to_string(),
            )
            .suggest(Suggestion::cmd("Refresh them", "rvn sync")),
        );
    }
}

/// Turn a database error into the repair it actually calls for.
///
/// The wording comes from the tool, so this matches on the phrase that
/// identifies the fault rather than guessing from context.
fn db_repair_suggestions(failures: &[&crate::probe::packages::DbReader]) -> Vec<Suggestion> {
    let all: String = failures
        .iter()
        .filter_map(|r| r.error.as_deref())
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    let mut out = Vec::new();

    if all.contains("incorrect version") || all.contains("database version") {
        out.push(Suggestion::new(
            "The database on disk was written in a newer format than the installed library \
             expects, which is what an interrupted or partial upgrade leaves behind.",
        ));
        out.push(Suggestion::root_cmd(
            "Migrate the database to the format the installed tools expect",
            "pacman-db-upgrade",
        ));
    }

    if all.contains("unable to lock") || all.contains("db.lck") {
        out.push(Suggestion::cmd(
            "Confirm no package operation is actually running",
            "pgrep -a 'rvn|pacman|makepkg'",
        ));
    }

    if out.is_empty() {
        out.push(Suggestion::cmd(
            "See the full error with nothing suppressed",
            "pacman -Qq",
        ));
    }
    out
}

fn hardware(view: &SystemView, out: &mut Vec<Finding>) {
    let Some(h) = &view.hardware else { return };

    let mem = &h.memory;
    if mem.total_bytes > 0 && mem.used_percent() >= threshold::MEMORY_WARNING_PCT {
        let mut f = Finding::new(
            "hardware.memory-pressure",
            Severity::Warning,
            format!("Memory is {}% used", mem.used_percent()),
        )
        .evidence(format!(
            "{} available of {}",
            crate::ui::bytes(mem.available_bytes),
            crate::ui::bytes(mem.total_bytes)
        ));
        if mem.swap_total_bytes == 0 {
            f = f.evidence(
                "There is no swap, so the kernel's only remaining move is to kill a process."
                    .to_string(),
            );
        }
        f = f.suggest(Suggestion::cmd(
            "See what is holding it",
            "ps -eo pid,rss,comm --sort=-rss | head -12",
        ));
        out.push(f);
    }

    if let Some(swap) = mem.swap_used_percent()
        && swap >= threshold::SWAP_WARNING_PCT
    {
        out.push(
            Finding::new(
                "hardware.swapping",
                Severity::Warning,
                format!("Swap is {swap}% used"),
            )
            .evidence(format!(
                "{} of {} swap in use",
                crate::ui::bytes(mem.swap_total_bytes.saturating_sub(mem.swap_free_bytes)),
                crate::ui::bytes(mem.swap_total_bytes)
            ))
            .evidence(
                "Once a desktop is paging, every click waits on the disk. This is the usual \
                 cause of a machine that has become unresponsive rather than merely slow."
                    .to_string(),
            ),
        );
    }

    if let Some(per_core) = h.load_per_core()
        && per_core >= threshold::LOAD_PER_CORE
    {
        out.push(
            Finding::new(
                "hardware.load",
                Severity::Note,
                format!("Load is {per_core:.1} per core"),
            )
            .evidence(format!(
                "Load average {:?} across {} cores",
                h.load.unwrap_or_default(),
                h.cpu_count
            ))
            .evidence(
                "More runnable work than cores. If this is steady rather than a burst, something \
                 is busy that you did not start."
                    .to_string(),
            )
            .suggest(Suggestion::cmd(
                "See what is running",
                "ps -eo pid,pcpu,comm --sort=-pcpu | head -12",
            )),
        );
    }

    if let Some(t) = h.thermal_celsius
        && t >= threshold::THERMAL_WARNING_C
    {
        out.push(
            Finding::new(
                "hardware.thermal",
                Severity::Warning,
                format!("The hottest thermal zone reads {t:.0}°C"),
            )
            .evidence(
                "At this temperature the CPU is reducing its own clock, which feels like the \
                 machine getting slower for no reason."
                    .to_string(),
            ),
        );
    }

    for fw in &h.missing_firmware {
        out.push(
            Finding::new(
                "hardware.missing-firmware",
                Severity::Warning,
                "A device asked for firmware that is not installed",
            )
            .evidence(format!("The kernel could not load {fw}"))
            .evidence(
                "The device will be absent rather than broken: nothing above the kernel reports \
                 this, so the hardware simply appears not to exist."
                    .to_string(),
            )
            .suggest(Suggestion::cmd(
                "Find the package that carries it",
                "rvn find linux-firmware",
            )),
        );
    }

    // Raven's powerd steps the CPU down when the cord comes out, so "the
    // machine is slow" and "the machine is on battery" are the same sentence
    // more often than people expect. Worth saying once, and only when there is
    // a load to explain.
    if h.on_battery() && h.load_per_core().map(|l| l >= 1.0).unwrap_or(false) {
        out.push(
            Finding::new(
                "hardware.on-battery",
                Severity::Note,
                "The machine is busy while running on battery",
            )
            .evidence(format!(
                "Battery is discharging and load is {:.1} per core",
                h.load_per_core().unwrap_or(0.0)
            ))
            .evidence(
                "A power profile that steps the CPU down on battery is doing its job here.                  The same work plugged in will finish faster."
                    .to_string(),
            ),
        );
    }

    if let Some(b) = &h.battery
        && let Some(health) = b.health_percent
        && health < threshold::BATTERY_HEALTH_PCT
    {
        out.push(
            Finding::new(
                "hardware.battery-health",
                Severity::Note,
                format!("The battery holds {health}% of its original capacity"),
            )
            .evidence(format!(
                "{} reports full charge well below its design capacity",
                b.name
            ))
            .evidence(
                "Runtime scales with this directly. Nothing is misconfigured; the cell has aged."
                    .to_string(),
            ),
        );
    }
}

fn logs(view: &SystemView, out: &mut Vec<Finding>) {
    let Some(l) = &view.logs else { return };

    // Kernel messages that name a specific, actionable failure. The generic
    // "there are errors in the log" finding is deliberately absent: it is true
    // on every machine ever built and helps nobody.
    for line in &l.kernel {
        let low = line.to_ascii_lowercase();
        if low.contains("out of memory") || low.contains("oom-kill") {
            out.push(
                Finding::new(
                    "logs.oom",
                    Severity::Warning,
                    "The kernel has killed a process to reclaim memory",
                )
                .evidence(line.clone())
                .evidence(
                    "Whatever was killed disappeared without a crash dialog, which is why an \
                     application can vanish mid-task with nothing in its own log."
                        .to_string(),
                ),
            );
            break;
        }
    }

    for line in &l.kernel {
        let low = line.to_ascii_lowercase();
        if low.contains("i/o error") || low.contains("ata error") {
            out.push(
                Finding::new(
                    "logs.io-error",
                    Severity::Critical,
                    "A storage device reported an I/O error",
                )
                .evidence(line.clone())
                .evidence(
                    "Back up anything that matters before investigating. A disk that reports \
                     read errors may not survive the investigation."
                        .to_string(),
                )
                .suggest(Suggestion::cmd(
                    "Check the drive's own health counters",
                    "smartctl -H /dev/sda",
                )),
            );
            break;
        }
    }

    if !l.unreadable.is_empty() {
        out.push(
            Finding::new(
                "logs.unreadable",
                Severity::Note,
                "Some logs could not be read by this account",
            )
            .evidence(l.unreadable.join(", "))
            .evidence(
                "Oracle deliberately runs as you rather than as root, so this is expected. \
                 Anything in those files is simply outside what it can see."
                    .to_string(),
            ),
        );
    }
}

/// A one-line summary of a report, for the places that need a sentence rather
/// than a list.
pub fn summarise(findings: &[Finding]) -> String {
    let critical = findings
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .count();
    let warning = findings
        .iter()
        .filter(|f| f.severity == Severity::Warning)
        .count();
    let note = findings
        .iter()
        .filter(|f| f.severity == Severity::Note)
        .count();

    match (critical, warning, note) {
        (0, 0, 0) => "Nothing to report.".to_string(),
        (0, 0, n) => format!("{n} note{}, nothing wrong.", plural(n)),
        (0, w, 0) => format!("{w} warning{}.", plural(w)),
        (0, w, n) => format!("{w} warning{}, {n} note{}.", plural(w), plural(n)),
        (c, w, n) => {
            let mut parts = vec![format!("{c} critical")];
            if w > 0 {
                parts.push(format!("{w} warning{}", plural(w)));
            }
            if n > 0 {
                parts.push(format!("{n} note{}", plural(n)));
            }
            format!("{}.", parts.join(", "))
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{services, storage};

    fn view() -> SystemView {
        SystemView::default()
    }

    #[test]
    fn an_empty_view_produces_no_findings() {
        assert!(
            run(&view()).is_empty(),
            "silence on a machine we know nothing about"
        );
    }

    #[test]
    fn a_healthy_machine_stays_quiet() {
        let mut v = view();
        v.storage = Some(storage::Storage {
            filesystems: vec![storage::Filesystem {
                mount: "/".into(),
                device: "/dev/sda1".into(),
                fstype: "ext4".into(),
                total_bytes: 500 * 1024 * 1024 * 1024,
                available_bytes: 300 * 1024 * 1024 * 1024,
                used_percent: 40,
                inodes_used_percent: Some(12),
                read_only: false,
            }],
            ..Default::default()
        });
        v.services = Some(services::Services {
            manager: "raven-init".into(),
            raven: vec![],
            systemd_failed: vec![],
            error: None,
        });
        assert!(
            run(&v).is_empty(),
            "a rule that fires on a healthy machine trains people to ignore the report"
        );
    }

    #[test]
    fn a_full_root_filesystem_is_critical_and_says_where_to_look() {
        let mut v = view();
        v.storage = Some(storage::Storage {
            filesystems: vec![storage::Filesystem {
                mount: "/".into(),
                device: "/dev/sda1".into(),
                fstype: "ext4".into(),
                total_bytes: 100 * 1024 * 1024 * 1024,
                available_bytes: 1024 * 1024 * 1024,
                used_percent: 99,
                inodes_used_percent: None,
                read_only: false,
            }],
            ..Default::default()
        });
        let f = run(&v);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Critical);
        assert_eq!(f[0].id, "storage.full");
        assert!(f[0].suggestions.iter().any(|s| s.command.is_some()));
    }

    #[test]
    fn a_silently_failing_service_is_found_and_named() {
        let mut v = view();
        v.services = Some(services::Services {
            manager: "raven-init".into(),
            raven: vec![services::RavenService {
                name: "raven-dhcp".into(),
                description: "DHCP for wired links".into(),
                exec: "/usr/bin/raven-dhcp".into(),
                enabled: true,
                critical: false,
                restart: false,
                after: vec![],
                source: "/etc/raven/init.toml".into(),
                exec_present: false,
                ready_path: None,
                ready_present: None,
                log_errors: vec![],
                log_path: None,
                log_age_seconds: None,
            }],
            systemd_failed: vec![],
            error: None,
        });
        let f = run(&v);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, "services.exec-missing");
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].title.contains("raven-dhcp"));
        assert!(
            f[0].evidence.iter().any(|e| e.contains("not critical")),
            "the report must say why nobody was told"
        );
    }

    #[test]
    fn findings_come_back_worst_first_and_in_a_stable_order() {
        let mut v = view();
        v.storage = Some(storage::Storage {
            filesystems: vec![
                storage::Filesystem {
                    mount: "/".into(),
                    device: "/dev/sda1".into(),
                    fstype: "ext4".into(),
                    total_bytes: 100,
                    available_bytes: 1,
                    used_percent: 99,
                    inodes_used_percent: None,
                    read_only: false,
                },
                storage::Filesystem {
                    mount: "/home".into(),
                    device: "/dev/sda2".into(),
                    fstype: "ext4".into(),
                    total_bytes: 100,
                    available_bytes: 12,
                    used_percent: 88,
                    inodes_used_percent: None,
                    read_only: false,
                },
            ],
            ..Default::default()
        });
        let f = run(&v);
        assert_eq!(f[0].severity, Severity::Critical);
        assert_eq!(f[1].severity, Severity::Warning);

        let again = run(&v);
        let ids: Vec<&str> = f.iter().map(|x| x.id.as_str()).collect();
        let ids2: Vec<&str> = again.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ids2, "two runs must be diffable");
    }

    #[test]
    fn summaries_read_like_sentences() {
        assert_eq!(summarise(&[]), "Nothing to report.");
        let one = vec![Finding::new("x", Severity::Warning, "t")];
        assert_eq!(summarise(&one), "1 warning.");
        let two = vec![
            Finding::new("x", Severity::Warning, "t"),
            Finding::new("y", Severity::Warning, "t"),
        ];
        assert_eq!(summarise(&two), "2 warnings.");
        let mixed = vec![
            Finding::new("x", Severity::Critical, "t"),
            Finding::new("y", Severity::Note, "t"),
        ];
        assert_eq!(summarise(&mixed), "1 critical, 1 note.");
    }
}
