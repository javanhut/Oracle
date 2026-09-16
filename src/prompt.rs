//! Building what the model sees.
//!
//! Three jobs. Turn a `SystemView` into a compact description of the machine,
//! give the model instructions that keep it useful rather than enthusiastic,
//! and carry a conversation forward when one answer was not enough.
//!
//! Troubleshooting is rarely one question. The person tries the suggestion,
//! it half works, and the next thing they say is "that printed something
//! else". So a turn is assembled from the transcript so far plus the machine
//! as it is *now*: the context block is attached to the newest question only
//! and re-read before every turn, which keeps the token cost flat across a
//! long conversation and means the model sees the state after the last thing
//! the person ran rather than the state before it.
//!
//! The instructions matter more than they look. A small local model asked an
//! open question about a Linux system will confidently invent a systemd unit
//! name, a config file that does not exist, and a package from a distribution
//! this is not. The system prompt's main work is to hold it to the evidence it
//! was given and to make "I don't know" an acceptable answer, because on a
//! troubleshooting tool a confident wrong answer costs more than no answer.

use crate::config::Config;
use crate::model::Message;
use crate::probe::{Finding, SystemView};

/// Instructions for a troubleshooting answer.
pub fn system_prompt(view: &SystemView) -> String {
    let distro = if view.host.is_raven {
        "Raven Linux, an Arch-derived distribution with its own init (raven-init), \
         package manager (rvn), wireless daemon (cawd) and shell (ravenshell)"
    } else {
        "a Linux system"
    };

    let mut p = format!(
        "You are Oracle, a troubleshooting assistant running locally on {distro}. \
         You are talking to the person sitting at this machine.\n\n"
    );

    p.push_str(
        "How to answer:\n\
         - Lead with what is most likely wrong, in one sentence.\n\
         - Ground every claim in the system context you were given. If the context does not \
           show something, say you cannot see it rather than guessing.\n\
         - Suggest the smallest safe next step first: a command that inspects before a command \
           that changes.\n\
         - Put commands on their own line. Do not invent file paths, service names, or package \
           names. If you are unsure whether something exists on this system, say so.\n\
         - Say \"I don't know from what I can see here\" when that is the truth. It is a good \
           answer.\n\
         - Be brief. Three short paragraphs at most, fewer if the answer is simple.\n\n",
    );

    p.push_str(
        "This can take more than one turn:\n\
         - The person can reply after your answer. Earlier turns are the same conversation, so \
           do not start again from nothing or repeat an answer you have already given.\n\
         - The system context is re-read before every turn. If it now disagrees with something \
           you said earlier, the context is right and you were working from an older reading.\n\
         - If they paste what a command printed, that is fresh evidence about this machine. \
           Trust it over your own guess.\n\
         - If they say your suggestion did not work, do not suggest it again. Ask for the output \
           that proves what happened, or try a different explanation.\n\
         - Do not end with an offer to help further. They will ask if they want more.\n\n",
    );

    p.push_str(
        "What you must not do:\n\
         - Do not tell the user to run a command that destroys data, reinstalls the system, or \
           disables security, unless they asked for exactly that.\n\
         - Do not claim to have run anything. You cannot run commands; you can only suggest them.\n\
         - Do not offer to install or enable software on the user's behalf.\n",
    );

    if view.host.is_raven {
        p.push_str(
            "\nRaven-specific facts you can rely on:\n\
             - Services are TOML files in /etc/raven/init.toml and /etc/raven/init.d/, not \
               systemd units. `raven-rc` controls them.\n\
             - Packages: `rvn install`, `rvn uninstall`, `rvn update`, `rvn sync`, `rvn find`. \
               It is Arch-compatible and also builds from the AUR.\n\
             - `rvnd` is a root daemon on /run/rvn/ctl; members of the `wheel` group install \
               without a password and there is no polkit agent and no sudo prompt.\n\
             - Wireless is `caw` and `cawd`, not iwd, iwctl, or wpa_supplicant.\n\
             - Per-service logs are in /var/log/raven/<service>.log.\n\
             - A service with `critical = false` that fails does so silently.\n",
        );
    }

    p
}

/// Render the machine as text for the model.
///
/// Compact on purpose. A 3B model given four thousand tokens of system detail
/// answers worse than the same model given four hundred tokens of the right
/// detail, and the difference is almost entirely in what gets left out.
pub fn system_context(view: &SystemView, cfg: &Config) -> String {
    let mut s = String::new();

    s.push_str("=== This machine ===\n");
    s.push_str(&format!("Distribution: {}\n", view.host.distro));
    s.push_str(&format!("Kernel: {}\n", view.host.kernel));
    s.push_str(&format!("Init: {}\n", view.host.init));
    if view.host.uptime_seconds > 0 {
        s.push_str(&format!(
            "Uptime: {}\n",
            crate::sys::humanise_duration(view.host.uptime_seconds)
        ));
    }
    if let Some(d) = &view.host.desktop {
        s.push_str(&format!("Desktop: {d}\n"));
    }

    if let Some(h) = &view.hardware {
        s.push_str("\n=== Hardware ===\n");
        if !h.cpu_model.is_empty() {
            s.push_str(&format!("CPU: {} ({} cores)\n", h.cpu_model, h.cpu_count));
        }
        s.push_str(&format!(
            "Memory: {} available of {}\n",
            crate::ui::bytes(h.memory.available_bytes),
            crate::ui::bytes(h.memory.total_bytes)
        ));
        if h.memory.swap_total_bytes > 0 {
            s.push_str(&format!(
                "Swap: {} used of {}\n",
                crate::ui::bytes(
                    h.memory
                        .swap_total_bytes
                        .saturating_sub(h.memory.swap_free_bytes)
                ),
                crate::ui::bytes(h.memory.swap_total_bytes)
            ));
        }
        if let Some((one, five, fifteen)) = h.load {
            s.push_str(&format!("Load: {one:.2} {five:.2} {fifteen:.2}\n"));
        }
        if !h.gpu.is_empty() {
            s.push_str(&format!("Graphics: {}\n", h.gpu.join("; ")));
        }
        if let Some(b) = &h.battery {
            s.push_str(&format!(
                "Battery: {}%, {}\n",
                b.percent.unwrap_or(0),
                b.status
            ));
        }
        if let Some(v) = &h.virtualised {
            s.push_str(&format!("Virtualised: {v}\n"));
        }
        if !h.missing_firmware.is_empty() {
            s.push_str(&format!(
                "Firmware the kernel could not load: {}\n",
                h.missing_firmware.join(", ")
            ));
        }
    }

    if let Some(st) = &view.storage {
        s.push_str("\n=== Storage ===\n");
        if let Some(root) = st.root() {
            s.push_str(&format!(
                "Root filesystem is {}% full ({} free)\n",
                root.used_percent,
                crate::ui::bytes(root.available_bytes)
            ));
        }
        for fs in &st.filesystems {
            s.push_str(&format!(
                "{} ({}) {}% used, {} free{}\n",
                fs.mount,
                fs.fstype,
                fs.used_percent,
                crate::ui::bytes(fs.available_bytes),
                if fs.read_only { ", READ-ONLY" } else { "" }
            ));
        }
    }

    if let Some(n) = &view.network {
        s.push_str("\n=== Network ===\n");
        for i in n.interfaces.iter().filter(|i| !i.loopback) {
            s.push_str(&format!(
                "{}: {}{}{}, addresses: {}\n",
                i.name,
                i.operstate,
                if i.wireless { ", wireless" } else { "" },
                if i.carrier {
                    ", carrier"
                } else {
                    ", no carrier"
                },
                if i.addresses.is_empty() {
                    "none".to_string()
                } else {
                    i.addresses.join(" ")
                }
            ));
        }
        match &n.default_route {
            Some(r) => s.push_str(&format!(
                "Default route via {} on {}\n",
                r.gateway, r.interface
            )),
            None => s.push_str("No default route\n"),
        }
        s.push_str(&format!(
            "Nameservers: {}\n",
            if n.nameservers.is_empty() {
                "none configured".to_string()
            } else {
                n.nameservers.join(" ")
            }
        ));
        if let Some(running) = n.cawd_running {
            s.push_str(&format!(
                "cawd (wireless daemon): {}\n",
                if running { "running" } else { "not running" }
            ));
        }
    }

    if let Some(p) = &view.packages {
        s.push_str("\n=== Packages ===\n");
        s.push_str(&format!(
            "Manager: {}\n",
            p.manager.clone().unwrap_or_else(|| "none found".into())
        ));
        if let Some(sock) = p.rvnd_socket {
            s.push_str(&format!(
                "rvnd socket: {}; account in wheel: {}\n",
                if sock { "present" } else { "absent" },
                p.in_wheel
            ));
        }
        for r in p.db_failures() {
            s.push_str(&format!(
                "{} cannot read the local package database: {}\n",
                r.tool,
                r.error.as_deref().unwrap_or("no message given")
            ));
        }
        let working = p.db_successes();
        if !working.is_empty() && !p.db_failures().is_empty() {
            s.push_str(&format!(
                "These tools read it without trouble: {}\n",
                working
                    .iter()
                    .map(|r| r.tool.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(lock) = &p.stale_lock {
            s.push_str(&format!("A database lock is present at {lock}\n"));
        }
        if let Some(age) = p.sync_age_seconds {
            s.push_str(&format!(
                "Repository databases last synced {} ago\n",
                crate::sys::humanise_duration(age)
            ));
        }
    }

    if let Some(sv) = &view.services {
        s.push_str(&format!(
            "\n=== Services ===\nManager: {}, {} enabled\n",
            sv.manager,
            sv.enabled_count()
        ));
        let silent = sv.silent_failures();
        let not_ready = sv.not_ready();
        if !silent.is_empty() || !not_ready.is_empty() || !sv.systemd_failed.is_empty() {
            s.push_str("\n=== Services with problems ===\n");
            for svc in silent {
                s.push_str(&format!(
                    "{}: enabled, but its program {} is not installed (critical = {})\n",
                    svc.name, svc.exec, svc.critical
                ));
            }
            for svc in not_ready {
                s.push_str(&format!(
                    "{}: enabled, but never created {}\n",
                    svc.name,
                    svc.ready_path.as_deref().unwrap_or("its readiness path")
                ));
            }
            for u in &sv.systemd_failed {
                s.push_str(&format!("{}: failed ({})\n", u.unit, u.state));
            }
        }
    }

    if let Some(l) = &view.logs
        && !l.is_empty()
    {
        s.push_str(&format!(
            "\n=== Recent errors in logs ({} total) ===\n",
            l.total_errors()
        ));
        for src in l.raven.iter().take(6) {
            for line in src.errors.iter().take(3) {
                s.push_str(&format!("[{}] {line}\n", src.service));
            }
        }
        for line in l.kernel.iter().take(8) {
            s.push_str(&format!("[kernel] {line}\n"));
        }
        for line in l.journal.iter().take(8) {
            s.push_str(&format!("[journal] {line}\n"));
        }
    }

    if cfg.privacy.redact {
        crate::redact::scrub(&s)
    } else {
        s
    }
}

/// Findings rendered for the model, so it explains rather than re-derives.
///
/// The rules already found these with certainty. Handing them over stops the
/// model from rediscovering them badly, and gives it something concrete to
/// talk about instead of speculating.
pub fn findings_context(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "Automated checks found nothing wrong.\n".to_string();
    }
    let mut s = String::from("=== What automated checks already found ===\n");
    for f in findings {
        s.push_str(&format!("[{}] {}\n", f.severity.label(), f.title));
        for e in f.evidence.iter().take(3) {
            s.push_str(&format!("  evidence: {e}\n"));
        }
    }
    s
}

/// One finished turn: what the person said, and what the model said back.
///
/// The question is stored in the person's own voice rather than in the
/// wrapper the model was sent, because on a later turn the role already says
/// who was speaking and the wrapper only adds noise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    pub question: String,
    pub answer: String,
}

impl Exchange {
    pub fn new(question: impl Into<String>, answer: impl Into<String>) -> Exchange {
        Exchange {
            question: question.into(),
            answer: answer.into(),
        }
    }

    /// One line naming this turn, for a transcript on screen.
    ///
    /// A typed question is already one line. A pasted error or a finding is
    /// not, and its first line is the part the person will recognise.
    pub fn headline(&self) -> &str {
        self.question
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
    }
}

/// How many past turns are replayed.
///
/// A small local model given a long transcript starts answering the question
/// it was asked four turns ago. Six turns is more than most troubleshooting
/// takes and still leaves room for the context block, which is the part that
/// has to survive.
const MAX_PAST: usize = 6;

/// Assemble one turn: the transcript so far, then the newest question with
/// the machine attached to it.
///
/// `preamble` is everything that goes in front of the new question -- the
/// context block, and whatever framing the caller wants. Past turns are sent
/// bare, since they have already been answered and their context is spent.
pub fn build_turn(
    past: &[Exchange],
    preamble: &str,
    pending: &str,
    view: &SystemView,
) -> Vec<Message> {
    let mut messages = Vec::with_capacity(past.len() * 2 + 2);
    messages.push(Message::system(system_prompt(view)));

    let dropped = past.len().saturating_sub(MAX_PAST);
    for e in &past[dropped..] {
        messages.push(Message::user(e.question.clone()));
        messages.push(Message::assistant(e.answer.clone()));
    }

    let elision = if dropped > 0 {
        "Earlier turns in this conversation have been left out to save room. If you need \
         something from them, ask for it again.\n\n"
    } else {
        ""
    };
    messages.push(Message::user(format!("{elision}{preamble}{pending}")));
    messages
}

/// The context block that goes in front of a new question.
fn preamble(view: &SystemView, findings: Option<&[Finding]>, cfg: &Config) -> String {
    let context = system_context(view, cfg);
    match findings {
        Some(f) => format!("System context:\n\n{context}\n{}\n", findings_context(f)),
        None => format!("System context:\n\n{context}\n"),
    }
}

/// Assemble the conversation for a question.
pub fn build(
    question: &str,
    view: &SystemView,
    findings: &[Finding],
    cfg: &Config,
) -> Vec<Message> {
    build_turn(
        &[],
        &preamble(view, Some(findings), cfg),
        &format!("The person at this machine asks:\n\n{question}"),
        view,
    )
}

/// Assemble the next turn of a conversation that is already going.
///
/// The machine has been read again by the time this is called, which is the
/// point: between one turn and the next the person may well have run the
/// thing that was suggested.
pub fn build_follow_up(
    past: &[Exchange],
    question: &str,
    view: &SystemView,
    findings: &[Finding],
    cfg: &Config,
) -> Vec<Message> {
    let preamble = format!(
        "{}The context above was read again just now, after your last answer, so it shows this \
         machine as it is at this moment.\n\n",
        preamble(view, Some(findings), cfg)
    );
    build_turn(
        past,
        &preamble,
        &format!("The person at this machine replies:\n\n{question}"),
        view,
    )
}

/// Assemble the conversation for explaining an error someone pasted or piped
/// in.
pub fn build_explain(error_text: &str, view: &SystemView, cfg: &Config) -> Vec<Message> {
    build_turn(
        &[],
        &preamble(view, None, cfg),
        &format!(
            "{}\n\n\
             Explain what this error means and what is most likely causing it on this machine. \
             Then give the smallest next step. If the output does not contain enough to be sure, \
             say what else you would need to see.",
            pasted_error(error_text, cfg)
        ),
        view,
    )
}

/// A pasted error in the person's own voice, for the transcript.
///
/// Redaction happens here rather than at the call site so that what is stored
/// and replayed on later turns is the same scrubbed text the model was sent
/// the first time.
pub fn pasted_error(error_text: &str, cfg: &Config) -> String {
    let error_text = if cfg.privacy.redact {
        crate::redact::scrub(error_text)
    } else {
        error_text.to_string()
    };
    format!("I ran something that failed, and this is what it printed:\n\n---\n{error_text}\n---")
}

/// Assemble a request to expand on a single finding.
///
/// Only the interfaces have a notion of a *selected* finding. The command line
/// explains a whole report at once, through `build_report_summary`.
///
/// The rules already established what is wrong and why. The model's job here
/// is the part rules are bad at: what this means for the person, what to check
/// first, and what the risks of the suggested repair are.
pub fn build_finding_explanation(
    finding: &Finding,
    view: &SystemView,
    cfg: &Config,
) -> Vec<Message> {
    build_turn(
        &[],
        &preamble(view, None, cfg),
        &format!(
            "{}\n\n\
             Explain to the person at this machine what this means for them in practice, what \
             to check before acting, and anything that could go wrong with the suggested step. \
             Do not repeat the evidence back to them. Two short paragraphs at most.",
            finding_as_question(finding)
        ),
        view,
    )
}

/// A finding in the person's own voice, for the transcript.
pub fn finding_as_question(finding: &Finding) -> String {
    let mut detail = format!(
        "A check on this machine reported: {}\n\nSeverity: {}\n",
        finding.title,
        finding.severity.label()
    );
    if !finding.evidence.is_empty() {
        detail.push_str("Evidence:\n");
        for e in &finding.evidence {
            detail.push_str(&format!("  - {e}\n"));
        }
    }
    if !finding.suggestions.is_empty() {
        detail.push_str("Suggested steps already prepared:\n");
        for s in &finding.suggestions {
            detail.push_str(&format!("  - {}", s.what));
            if let Some(c) = &s.command {
                detail.push_str(&format!(" [{c}]"));
            }
            detail.push('\n');
        }
    }
    detail
}

/// Assemble a request to narrate a `doctor` report.
pub fn build_report_summary(findings: &[Finding], view: &SystemView, cfg: &Config) -> Vec<Message> {
    build_turn(
        &[],
        &preamble(view, Some(findings), cfg),
        "Write a short plain-language summary for the person at this machine: what is worth \
         their attention, in what order, and what is safe to ignore. Do not repeat the list \
         back to them. Two short paragraphs at most. If nothing needs attention, say so in \
         one sentence.",
        view,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{HostFacts, Severity};

    fn raven_view() -> SystemView {
        SystemView {
            host: HostFacts {
                kernel: "6.17.11-raven".into(),
                distro: "Raven Linux".into(),
                is_raven: true,
                init: "raven-init".into(),
                uptime_seconds: 3600,
                running_as_root: false,
                desktop: None,
            },
            ..Default::default()
        }
    }

    #[test]
    fn the_raven_prompt_names_raven_tools_and_not_systemd_ones() {
        let p = system_prompt(&raven_view());
        assert!(p.contains("raven-init"));
        assert!(p.contains("rvn"));
        assert!(p.contains("caw"));
        assert!(p.contains("not systemd units"));
    }

    #[test]
    fn a_non_raven_machine_gets_no_raven_claims() {
        let mut v = raven_view();
        v.host.is_raven = false;
        let p = system_prompt(&v);
        assert!(!p.contains("raven-init"));
    }

    #[test]
    fn the_prompt_permits_not_knowing() {
        let p = system_prompt(&raven_view());
        assert!(
            p.contains("don't know"),
            "a wrong confident answer is the failure mode"
        );
    }

    #[test]
    fn the_prompt_forbids_claiming_to_have_run_things() {
        let p = system_prompt(&raven_view());
        assert!(p.contains("cannot run commands"));
    }

    #[test]
    fn context_is_redacted_when_the_config_says_so() {
        let mut cfg = Config::default();
        cfg.privacy.redact = true;

        let mut v = raven_view();
        v.network = Some(crate::probe::network::Network {
            interfaces: vec![crate::probe::network::Interface {
                name: "wlan0".into(),
                operstate: "up".into(),
                carrier: true,
                wireless: true,
                loopback: false,
                addresses: vec!["93.184.216.34/24".into()],
                mtu: Some(1500),
                rx_bytes: 0,
                tx_bytes: 0,
            }],
            ..Default::default()
        });

        let text = system_context(&v, &cfg);
        assert!(!text.contains("93.184.216.34"), "got {text}");

        cfg.privacy.redact = false;
        assert!(system_context(&v, &cfg).contains("93.184.216.34"));
    }

    #[test]
    fn findings_are_handed_over_rather_than_left_to_be_rediscovered() {
        let f = vec![
            Finding::new("storage.full", Severity::Critical, "/ is 99% full")
                .evidence("1G free of 100G"),
        ];
        let text = findings_context(&f);
        assert!(text.contains("critical"));
        assert!(text.contains("99% full"));
        assert!(text.contains("1G free"));
    }

    #[test]
    fn no_findings_says_so_plainly() {
        assert!(findings_context(&[]).contains("nothing wrong"));
    }

    #[test]
    #[cfg(feature = "tui")]
    fn a_finding_explanation_carries_the_evidence_and_asks_for_practical_help() {
        let cfg = Config::default();
        let f = Finding::new("storage.full", Severity::Critical, "/ is 99% full")
            .evidence("1G free of 100G")
            .suggest(crate::probe::Suggestion::cmd("look", "du -sh /"));
        let msgs = build_finding_explanation(&f, &raven_view(), &cfg);
        assert_eq!(msgs.len(), 2);
        let body = &msgs[1].content;
        assert!(body.contains("/ is 99% full"));
        assert!(body.contains("1G free of 100G"));
        assert!(body.contains("du -sh /"));
        assert!(body.contains("could go wrong"));
    }

    #[test]
    fn a_follow_up_replays_the_conversation_as_user_and_assistant_turns() {
        use crate::model::Role;
        let cfg = Config::default();
        let past = vec![
            Exchange::new("why is it slow", "swap is thrashing"),
            Exchange::new("how do I stop that", "close the browser"),
        ];
        let msgs = build_follow_up(&past, "did that work", &raven_view(), &[], &cfg);

        let roles: Vec<&Role> = msgs.iter().map(|m| &m.role).collect();
        assert_eq!(
            roles,
            vec![
                &Role::System,
                &Role::User,
                &Role::Assistant,
                &Role::User,
                &Role::Assistant,
                &Role::User
            ]
        );
        assert_eq!(msgs[1].content, "why is it slow");
        assert_eq!(msgs[2].content, "swap is thrashing");
        assert!(msgs.last().unwrap().content.contains("did that work"));
    }

    #[test]
    fn only_the_newest_turn_carries_the_machine() {
        // The context block is the expensive part. Repeating it every turn
        // would crowd out the conversation on a small model, and the older
        // copies would be stale anyway.
        let cfg = Config::default();
        let past = vec![Exchange::new("why is it slow", "swap is thrashing")];
        let msgs = build_follow_up(&past, "did that work", &raven_view(), &[], &cfg);

        assert!(!msgs[1].content.contains("=== This machine ==="));
        let newest = &msgs.last().unwrap().content;
        assert!(newest.contains("=== This machine ==="));
        assert!(
            newest.contains("read again just now"),
            "the model has to know the reading is current, or it will answer from the first one"
        );
    }

    #[test]
    fn a_long_conversation_is_cut_down_and_the_model_is_told_it_was() {
        let cfg = Config::default();
        let past: Vec<Exchange> = (0..MAX_PAST + 3)
            .map(|i| Exchange::new(format!("question {i}"), format!("answer {i}")))
            .collect();
        let msgs = build_follow_up(&past, "and now", &raven_view(), &[], &cfg);

        assert_eq!(msgs.len(), MAX_PAST * 2 + 2);
        assert_eq!(msgs[1].content, "question 3", "the oldest turns go first");
        assert!(msgs.last().unwrap().content.contains("left out"));
    }

    #[test]
    fn a_turn_is_named_on_screen_by_its_first_real_line() {
        let cfg = Config::default();
        assert_eq!(
            Exchange::new("why is it slow", "").headline(),
            "why is it slow"
        );
        // A pasted error is many lines; the first one is what the person will
        // recognise in a transcript.
        let pasted = Exchange::new(pasted_error("error: db is locked\n  at line 4", &cfg), "");
        assert_eq!(
            pasted.headline(),
            "I ran something that failed, and this is what it printed:"
        );
    }

    #[test]
    fn a_built_conversation_is_a_system_turn_then_one_user_turn() {
        let cfg = Config::default();
        let msgs = build("why is it slow", &raven_view(), &[], &cfg);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, crate::model::Role::System);
        assert_eq!(msgs[1].role, crate::model::Role::User);
        assert!(msgs[1].content.contains("why is it slow"));
    }
}
