//! Oracle -- an opt-in local troubleshooting companion.
//!
//! # What this is
//!
//! A command-line helper that reads a Linux system, says what looks wrong, and
//! suggests what to try. It knows about Raven Linux specifically -- raven-init
//! services, rvn packages, caw wireless -- and works on any Linux without
//! them.
//!
//! # What this is not
//!
//! It is not part of Raven Linux. Nothing in the base system requires it,
//! references it, or knows it exists. It ships in no install profile, adds no
//! service to `/etc/raven/init.d`, writes no `session.d` entry, and has no
//! autostart entry. Its desktop app, `raven-oracle`, sits in the launcher like
//! any other Raven app and runs only while its window is open. Somebody who
//! never installs it has an unchanged system, and somebody who uninstalls it
//! has an unchanged system again.
//!
//! The restraint is the design, not a limitation of it. A troubleshooting
//! assistant that starts at login, watches your machine and offers advice is a
//! different and much worse product: it is the thing people turn off in the
//! first week. This one waits to be asked.
//!
//! # The rules it holds to
//!
//! 1. **It runs only when run.** No daemon, no timer, no autostart, no
//!    notifications. There is no code here that could add one.
//! 2. **It never changes the system.** It prints commands; the person decides.
//!    Every probe is a read.
//! 3. **It is useful without a model.** The rules in `diagnose` find real
//!    problems with no inference server, no weights and no network.
//! 4. **Nothing leaves the machine unless you say so.** The config refuses a
//!    non-loopback endpoint until it is permitted, http or https alike.
//!    `oracle context` shows exactly what would be sent, before anything is.
//! 5. **It does not nag.** No first-run wizard, no upsell at the end of
//!    commands, no telling you twice about the same missing piece.

mod cli;
#[cfg(feature = "tui")]
mod tui;

// The core lives in the library, shared with the desktop app. Importing it at
// the crate root keeps every `crate::probe` path in this binary working.
use oracle::{config, diagnose, model, probe, prompt, report, setup, stt, sys, ui};

use cli::{AskArgs, Command, DoctorArgs, ExplainArgs, VoiceCommand};
use config::Config;
use model::ModelError;
use probe::{ProbeOptions, Severity, SystemView};
use std::io::{IsTerminal, Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let parsed = match cli::parse(args) {
        Ok(p) => p,
        Err(e) => {
            ui::init(false);
            ui::error(&e);
            std::process::exit(2);
        }
    };

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            ui::init(parsed.global.plain);
            ui::error(&e);
            ui::note("Fix the file, or delete it to go back to the defaults.");
            std::process::exit(2);
        }
    };

    ui::init(parsed.global.plain || cfg.behaviour.plain);
    ui::set_quiet(parsed.global.quiet);

    // Oracle is designed to run as the person using the machine. As root it
    // reads more than it was meant to, and every suggestion it prints loses
    // the `sudo` that would have made the privilege boundary visible. It still
    // works; it says so once and carries on.
    if sys::is_root() && !matches!(parsed.command, Command::Version | Command::Help(_)) {
        ui::warn(
            "running as root. Oracle is meant to run as you -- it reads more this way, and it \
             cannot show you which suggestions need privilege.",
        );
    }

    std::process::exit(dispatch(parsed.command, cfg));
}

fn dispatch(command: Command, mut cfg: Config) -> i32 {
    match command {
        Command::Overview => {
            print!("{}", cli::OVERVIEW);
            0
        }
        Command::Help(topic) => {
            match topic.as_deref() {
                Some("voice") => print_voice_help(),
                _ => print!("{}", cli::HELP),
            }
            0
        }
        Command::Version => {
            println!("oracle {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Command::Doctor(args) => doctor(args, &cfg),
        Command::Ask(args) => ask(args, &cfg),
        Command::Explain(args) => explain(args, &cfg),
        Command::Context { json } => context(json, &cfg),
        Command::Tui => open_tui(&cfg),
        Command::Status => status(&cfg),
        Command::Setup => setup::run(&mut cfg),
        Command::Voice(sub) => voice(sub, &mut cfg),
        Command::Listen { seconds } => listen(seconds, &cfg),
        Command::Dictate { seconds } => dictate(seconds, &cfg),
        Command::Forget { assume_yes } => forget(assume_yes),
    }
}

// ---------------------------------------------------------------- doctor

fn doctor(args: DoctorArgs, cfg: &Config) -> i32 {
    let opts = ProbeOptions {
        online_checks: args.online,
        slow_checks: args.slow,
    };

    let view = if args.areas.is_empty() {
        SystemView::gather(cfg, opts)
    } else {
        SystemView::gather_for(cfg, &args.areas, opts)
    };
    let findings = diagnose::run(&view);

    if args.json {
        println!("{}", report::json(&view, &findings, cfg.privacy.redact));
        return exit_code(&findings, args.strict);
    }

    ui::info(&report::verdict(&findings));
    if !findings.is_empty() {
        println!();
        report::print(&findings, args.verbose);
    }

    // Say what was deliberately not looked at, so an empty report is never
    // mistaken for a clean bill of health it did not earn.
    let skipped = skipped_areas(cfg, &args.areas);
    if !skipped.is_empty() {
        println!();
        ui::note(&format!("Not checked: {}.", skipped.join(", ")));
    }
    // Only worth saying when the network was part of what was looked at.
    let looked_at_network = view.network.is_some();
    if !args.online && looked_at_network {
        ui::note("No network checks were made. `--online` permits them.");
    }

    if args.explain {
        explain_findings(&view, &findings, cfg);
    } else if !findings.is_empty() && has_model(cfg) {
        ui::note("`oracle doctor --explain` has the local model summarise this in plain language.");
    }

    exit_code(&findings, args.strict)
}

fn exit_code(findings: &[probe::Finding], strict: bool) -> i32 {
    if !strict {
        // A diagnostic that fails a script because it found a note is a
        // diagnostic people stop running.
        return 0;
    }
    if report::count_at_least(findings, Severity::Critical) > 0 {
        2
    } else if report::count_at_least(findings, Severity::Warning) > 0 {
        1
    } else {
        0
    }
}

fn skipped_areas(cfg: &Config, requested: &[probe::Area]) -> Vec<&'static str> {
    let c = &cfg.context;
    probe::Area::all()
        .into_iter()
        .filter(|a| {
            let enabled_in_config = match a {
                probe::Area::Services => c.services,
                probe::Area::Storage => c.storage,
                probe::Area::Network => c.network,
                probe::Area::Packages => c.packages,
                probe::Area::Logs => c.logs,
                probe::Area::Hardware => c.hardware,
            };
            let requested_here = requested.is_empty() || requested.contains(a);
            !(enabled_in_config && requested_here)
        })
        .map(|a| a.name())
        .collect()
}

fn explain_findings(view: &SystemView, findings: &[probe::Finding], cfg: &Config) {
    let messages = prompt::build_report_summary(findings, view, cfg);
    println!();
    match stream_answer(&messages, cfg) {
        Ok(()) => {}
        Err(e) => report_model_error(&e, cfg),
    }
}

// ------------------------------------------------------------------- ask

fn ask(args: AskArgs, cfg: &Config) -> i32 {
    let areas = if args.areas.is_empty() {
        probe::areas_for_question(&args.question)
    } else {
        args.areas.clone()
    };

    let opts = ProbeOptions {
        online_checks: args.online,
        slow_checks: false,
    };

    let reading = if areas.is_empty() {
        "Reading the system…".to_string()
    } else {
        format!(
            "Reading {}…",
            areas
                .iter()
                .map(|a| a.name())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    ui::note(&reading);

    let view = SystemView::gather_for(cfg, &areas, opts);
    let findings = diagnose::run(&view);
    let messages = prompt::build(&args.question, &view, &findings, cfg);

    if args.dry_run {
        for m in &messages {
            println!("--- {} ---", m.role.as_str());
            println!("{}", m.content);
        }
        return 0;
    }

    // Findings first. They are certain, and they are the answer often enough
    // that the model's paragraph is a bonus rather than the product.
    if !findings.is_empty() {
        ui::info(&report::verdict(&findings));
        println!();
        report::print(&findings, false);
        println!();
    }

    match stream_answer(&messages, cfg) {
        Ok(()) => 0,
        Err(e) => {
            if findings.is_empty() {
                report_model_error(&e, cfg);
                1
            } else {
                // The checks already said something useful, so a missing model
                // is a footnote rather than a failure.
                ui::note(&format!("No model answer: {e}"));
                0
            }
        }
    }
}

// --------------------------------------------------------------- explain

fn explain(args: ExplainArgs, cfg: &Config) -> i32 {
    let text = if args.text.trim().is_empty() {
        if std::io::stdin().is_terminal() {
            ui::error("nothing to explain.");
            ui::info("Pipe an error in, or pass it as an argument:");
            ui::command("rvn install foo 2>&1 | oracle explain");
            ui::command("oracle explain \"error: failed to initialize alpm library\"");
            return 2;
        }
        let mut buf = String::new();
        if std::io::stdin().read_to_string(&mut buf).is_err() {
            ui::error("could not read stdin");
            return 1;
        }
        buf
    } else {
        args.text.clone()
    };

    if text.trim().is_empty() {
        ui::error("the input was empty");
        return 2;
    }

    // Guess the subject from the error text itself, the same way `ask` guesses
    // from a question.
    let areas = probe::areas_for_question(&text);
    let view = SystemView::gather_for(cfg, &areas, ProbeOptions::default());
    let messages = prompt::build_explain(&text, &view, cfg);

    if args.dry_run {
        for m in &messages {
            println!("--- {} ---", m.role.as_str());
            println!("{}", m.content);
        }
        return 0;
    }

    match stream_answer(&messages, cfg) {
        Ok(()) => 0,
        Err(e) => {
            report_model_error(&e, cfg);
            // Fall back to the rules: they may well have found the cause.
            //
            // Only the serious findings, and only a few. Someone who pasted an
            // error wants that error explained; printing every note the
            // machine happens to have buries the one finding that matters.
            let relevant: Vec<probe::Finding> = diagnose::run(&view)
                .into_iter()
                .filter(|f| f.severity <= Severity::Warning)
                .take(3)
                .collect();
            if !relevant.is_empty() {
                println!();
                ui::info("What the built-in checks found on their own:");
                println!();
                report::print(&relevant, false);
            }
            1
        }
    }
}

// --------------------------------------------------------------- context

/// Show exactly what would be sent to a model.
///
/// This exists so the privacy claims are checkable rather than believable. No
/// model is contacted and nothing is sent; the text printed here is the text
/// that would be.
fn context(json: bool, cfg: &Config) -> i32 {
    let view = SystemView::gather(cfg, ProbeOptions::default());
    let findings = diagnose::run(&view);

    if json {
        println!("{}", report::json(&view, &findings, cfg.privacy.redact));
        return 0;
    }

    ui::note(&format!(
        "This is everything Oracle would send to a model, with redaction {}. \
         Nothing has been sent.",
        if cfg.privacy.redact { "on" } else { "OFF" }
    ));
    println!();
    println!("{}", prompt::system_context(&view, cfg));
    println!("{}", prompt::findings_context(&findings));
    0
}

/// Open the terminal interface, or explain that this build has none.
///
/// The interface is a Cargo feature so a minimal image can carry the checks
/// without a hundred crates of terminal machinery. A build without it says so
/// and points at the command that does the same job.
#[cfg(feature = "tui")]
fn open_tui(cfg: &Config) -> i32 {
    tui::run(cfg)
}

#[cfg(not(feature = "tui"))]
fn open_tui(_cfg: &Config) -> i32 {
    ui::error("this build of Oracle has no terminal interface.");
    ui::info(
        "It was compiled with --no-default-features. `oracle doctor` reports the same findings.",
    );
    2
}

// ---------------------------------------------------------------- status

fn status(cfg: &Config) -> i32 {
    ui::heading("Oracle");
    println!("  Version        {}", env!("CARGO_PKG_VERSION"));
    println!(
        "  Config         {}",
        if Config::exists() {
            config::tilde(&Config::path())
        } else {
            format!("none ({} defaults)", ui::dim("using"))
        }
    );
    println!("  Autostart      {}", ui::green("none, by design"));
    println!(
        "  Background     {}",
        ui::green("nothing runs when you are not running it")
    );
    println!(
        "  Changes        {}",
        if cfg.behaviour.suggest_only {
            ui::green("suggests only, never acts")
        } else {
            ui::yellow("suggests only, never acts")
        }
    );

    ui::heading("Model");
    match model::backend_for(cfg) {
        Ok(backend) => {
            let a = backend.availability();
            println!("  Backend        {}", backend.name());
            println!("  Endpoint       {}", a.endpoint);
            println!(
                "  Reachable      {}",
                if a.reachable {
                    ui::green("yes")
                } else {
                    ui::yellow("no")
                }
            );
            if let Some(m) = &a.selected {
                println!("  Would use      {m}");
            }
            if !a.models.is_empty() {
                println!("  Installed      {}", a.models.join(", "));
            }
            if let Some(d) = &a.detail {
                println!("  Note           {d}");
            }
        }
        Err(e) => {
            println!("  Backend        {}", ui::dim("none"));
            println!("  Note           {e}");
            println!("  {}", ui::dim("`oracle doctor` works without a model."));
        }
    }

    ui::heading("Dictation");
    print_voice_status(cfg);

    ui::heading("Privacy");
    println!(
        "  Redaction      {}",
        if cfg.privacy.redact {
            ui::green("on")
        } else {
            ui::yellow("off")
        }
    );
    println!(
        "  Remote model   {}",
        if cfg.privacy.allow_remote_endpoint {
            ui::yellow("permitted")
        } else {
            ui::green("refused")
        }
    );
    println!(
        "  History        {}",
        if cfg.privacy.keep_history {
            ui::yellow("kept")
        } else {
            ui::green("not kept")
        }
    );
    println!(
        "  {}",
        ui::dim("`oracle context` prints exactly what a model would be sent.")
    );

    let skipped = skipped_areas(cfg, &[]);
    if !skipped.is_empty() {
        ui::heading("Switched off in config");
        println!("  {}", skipped.join(", "));
    }

    0
}

// ----------------------------------------------------------------- voice

fn voice(sub: VoiceCommand, cfg: &mut Config) -> i32 {
    match sub {
        VoiceCommand::Status => {
            ui::heading("Dictation");
            print_voice_status(cfg);
            0
        }

        VoiceCommand::Models => {
            ui::heading("Whisper models");
            ui::info("  Fetch one with `oracle voice fetch <name>`.");
            println!();
            for m in stt::WHISPER_MODELS {
                let here = stt::models_dir().join(m.file).exists();
                println!(
                    "  {:<16} {:>8}  {}{}",
                    m.name,
                    m.size,
                    m.note,
                    if here {
                        ui::green("  [installed]")
                    } else {
                        String::new()
                    }
                );
            }
            println!();
            ui::note(&format!(
                "Stored in {}. They are plain files; delete any you do not want.",
                config::tilde(&stt::models_dir())
            ));
            0
        }

        VoiceCommand::Fetch { name, assume_yes } => setup::fetch_whisper_model(&name, assume_yes),

        VoiceCommand::Setup => {
            let mut probe = cfg.voice.clone();
            probe.enabled = true;
            match stt::Voice::resolve_ignoring_enabled(&probe) {
                Ok(v) => {
                    ui::info(&format!("{} {}", ui::green("ready"), v.describe()));
                    if cfg.voice.enabled {
                        ui::info("Dictation is already on.");
                        return 0;
                    }
                    if !ui::confirm("Turn dictation on for this account?") {
                        ui::info("Left off.");
                        return 0;
                    }
                    cfg.voice.enabled = true;
                    match cfg.save() {
                        Ok(p) => {
                            ui::info(&format!("On. Written to {}", config::tilde(&p)));
                            ui::info("Try:");
                            ui::command("oracle dictate");
                            0
                        }
                        Err(e) => {
                            ui::error(&e);
                            1
                        }
                    }
                }
                Err(e) => {
                    ui::info(&format!("{} {e}", ui::yellow("not ready")));
                    println!();
                    setup::print_voice_install_help();
                    1
                }
            }
        }

        VoiceCommand::Test => {
            let v = match stt::Voice::resolve(cfg) {
                Ok(v) => v,
                Err(e) => {
                    ui::error(&e.to_string());
                    return 1;
                }
            };
            ui::info(&format!("Using {}", v.describe()));
            match v.capture(Some(5)) {
                Ok(text) => {
                    ui::info("");
                    ui::info(&format!("Heard: {}", ui::bold(&text)));
                    ui::note("The recording has been deleted.");
                    0
                }
                Err(e) => {
                    ui::error(&e.to_string());
                    1
                }
            }
        }
    }
}

fn print_voice_status(cfg: &Config) {
    println!(
        "  Enabled        {}",
        if cfg.voice.enabled {
            ui::green("yes")
        } else {
            ui::dim("no")
        }
    );
    println!(
        "  Listening      {}",
        ui::green("only while a command runs")
    );

    let mut probe = cfg.voice.clone();
    probe.enabled = true;
    match stt::Voice::resolve_ignoring_enabled(&probe) {
        Ok(v) => println!(
            "  Pieces         {} {}",
            ui::green("all present:"),
            v.describe()
        ),
        Err(e) => println!("  Pieces         {} {e}", ui::yellow("incomplete:")),
    }
}

fn print_voice_help() {
    println!(
        "\
oracle voice -- dictation, entirely optional

  oracle voice status               what is present and what is missing
  oracle voice models               whisper models, with sizes
  oracle voice fetch <name>         download one, after telling you the size
  oracle voice setup                turn dictation on for this account
  oracle voice test                 record five seconds and show the transcript

  oracle dictate                    transcribe speech to stdout, nothing else
  oracle listen                     dictate a question, confirm it, then ask it

HOW IT BEHAVES
  Recording happens only while one of those commands is running, and the
  terminal says so while it does. There is no hotword and no background
  listener; that is not a setting that exists. The audio file is deleted as
  soon as it has been transcribed, including when transcription fails, and the
  transcript is shown to you before it is used for anything.

  Transcription is whisper.cpp by default, running on this machine. Set
  [voice] command in the config to use something else -- anything that takes a
  WAV path and prints text will do.
"
    );
}

// ------------------------------------------------------ listen / dictate

fn dictate(seconds: Option<u32>, cfg: &Config) -> i32 {
    let v = match stt::Voice::resolve(cfg) {
        Ok(v) => v,
        Err(e) => {
            ui::error(&e.to_string());
            if matches!(e, stt::VoiceError::Disabled) {
                ui::note("Dictation is opt-in and off until you turn it on.");
            }
            return 1;
        }
    };

    match v.capture(seconds) {
        Ok(text) => {
            // stdout is the transcript and nothing else, so this composes:
            //   oracle dictate | wl-copy
            println!("{text}");
            0
        }
        Err(e) => {
            ui::error(&e.to_string());
            1
        }
    }
}

fn listen(seconds: Option<u32>, cfg: &Config) -> i32 {
    let v = match stt::Voice::resolve(cfg) {
        Ok(v) => v,
        Err(e) => {
            ui::error(&e.to_string());
            return 1;
        }
    };

    let question = match v.capture(seconds) {
        Ok(t) => t,
        Err(e) => {
            ui::error(&e.to_string());
            return 1;
        }
    };

    ui::info("");
    ui::info(&format!("Heard: {}", ui::bold(&question)));

    // The transcript is confirmed before it becomes a question. Whisper
    // mishears, and asking about the wrong thing wastes more of someone's time
    // than one keystroke does.
    if std::io::stdin().is_terminal() && !ui::confirm("Ask that?") {
        ui::info("Dropped.");
        return 0;
    }

    ask(
        AskArgs {
            question,
            ..Default::default()
        },
        cfg,
    )
}

// ---------------------------------------------------------------- forget

/// Remove everything Oracle has written.
///
/// Leaving cleanly is part of being optional. This deletes the config and any
/// models that were fetched, then says plainly that the binary is still there
/// and how to remove it -- Oracle does not delete its own executable, because
/// a program that can remove itself from a system is a program that can be
/// made to.
fn forget(assume_yes: bool) -> i32 {
    let config_path = Config::path();
    let state = config::state_dir();

    let mut targets: Vec<std::path::PathBuf> = Vec::new();
    if config_path.exists() {
        targets.push(config_path.clone());
    }
    if state.exists() {
        targets.push(state.clone());
    }

    if targets.is_empty() {
        ui::info("Oracle has written nothing on this account. There is nothing to forget.");
        return 0;
    }

    ui::info("This will delete:");
    for t in &targets {
        let size = if t.is_dir() {
            sys::quick("du", &["-sh", &t.to_string_lossy()])
                .and_then(|o| {
                    o.text()
                        .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        ui::info(&format!("  {} {}", config::tilde(t), ui::dim(&size)));
    }
    ui::info("");

    if !assume_yes && !ui::confirm("Delete these?") {
        ui::info("Nothing deleted.");
        return 0;
    }

    let mut failed = false;
    for t in &targets {
        let r = if t.is_dir() {
            std::fs::remove_dir_all(t)
        } else {
            std::fs::remove_file(t)
        };
        if let Err(e) = r {
            ui::error(&format!("could not remove {}: {e}", t.display()));
            failed = true;
        }
    }

    if !failed {
        ui::info("Done. Oracle has nothing on this account.");
        ui::info("");
        ui::info("The binary is still installed. To remove it as well:");
        ui::command("imlazy uninstall    # from the source tree");
        ui::command("rm /usr/local/bin/oracle /usr/local/bin/raven-oracle");
    }
    if failed { 1 } else { 0 }
}

// ------------------------------------------------------------- model I/O

fn has_model(cfg: &Config) -> bool {
    model::backend_for(cfg)
        .map(|b| b.availability().reachable)
        .unwrap_or(false)
}

/// Send a conversation and print the reply as it arrives.
fn stream_answer(messages: &[model::Message], cfg: &Config) -> Result<(), ModelError> {
    let backend = model::backend_for(cfg).map_err(|_| ModelError::NotConfigured)?;

    let mut stdout = std::io::stdout();
    let mut started = false;
    let mut wrote_anything = false;

    let result = backend.chat(messages, &mut |chunk| {
        if !started {
            started = true;
        }
        wrote_anything = true;
        let _ = stdout.write_all(chunk.as_bytes());
        let _ = stdout.flush();
        true
    });

    if wrote_anything {
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }

    result.map(|_| ())
}

fn report_model_error(e: &ModelError, cfg: &Config) {
    match e {
        ModelError::NotConfigured | ModelError::Unreachable(_) | ModelError::NoModel(_) => {
            ui::error(&e.to_string());
            ui::info(&model::no_model_advice(cfg));
        }
        ModelError::Interrupted => ui::note("Stopped."),
        ModelError::Failed(s) => ui::error(s),
    }
}

/// Tests that guard the promises in the module documentation above.
///
/// These are unusual: they read the project's own install files rather than
/// its code. They exist because "Oracle runs only when you open it" is the
/// claim the whole design rests on, and it is the claim most likely to be
/// broken later by a well-meaning change that adds "just a small service file"
/// or an autostart entry to the install target. A launcher entry is fine -- it
/// is how an app is opened. Anything that starts Oracle without being asked is
/// not. A promise nobody can accidentally break is worth a strange-looking
/// test.
#[cfg(test)]
mod promises {
    const LAZY: &str = include_str!("../lazy.toml");
    const MAKEFILE: &str = include_str!("../Makefile");
    const DESKTOP_ENTRY: &str = include_str!("../data/com.ravenoracle.Raven.desktop");

    /// Paths whose appearance in an install target would mean Oracle had
    /// started integrating itself into the system.
    const FORBIDDEN: [&str; 8] = [
        "/etc/raven",
        "session.d",
        "init.d",
        "systemctl",
        "raven-rc enable",
        "autostart",
        "dbus-1/services",
        "systemd/user",
    ];

    #[test]
    fn the_install_targets_add_nothing_that_starts_on_its_own() {
        for (name, text) in [("lazy.toml", LAZY), ("Makefile", MAKEFILE)] {
            for line in text.lines() {
                // The files talk *about* not doing this, in comments and in
                // the message printed after installing. Only real commands
                // matter.
                let trimmed = line.trim_start();
                if trimmed.starts_with('#')
                    || trimmed.starts_with("@echo")
                    || trimmed.starts_with("\"echo")
                {
                    continue;
                }
                for bad in FORBIDDEN {
                    assert!(
                        !line.contains(bad),
                        "{name} touches {bad:?} in: {line}\n\
                         Oracle runs when it is opened. Anything that starts it otherwise makes \
                         it part of the system."
                    );
                }
            }
        }
    }

    #[test]
    fn the_launcher_entry_opens_the_app_and_does_nothing_else() {
        assert!(DESKTOP_ENTRY.contains("Exec=raven-oracle\n"));
        for bad in [
            "Autostart",
            "DBusActivatable",
            "X-GNOME-AutoRestart",
            "Hidden=",
        ] {
            assert!(
                !DESKTOP_ENTRY.contains(bad),
                "the desktop entry must not carry {bad:?}"
            );
        }
    }

    #[test]
    fn both_install_paths_stay_under_usr_local() {
        assert!(LAZY.contains("/usr/local"));
        assert!(MAKEFILE.contains("/usr/local"));
    }

    #[test]
    fn uninstalling_is_documented_in_both_build_files() {
        assert!(LAZY.contains("uninstall"));
        assert!(MAKEFILE.contains("uninstall:"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_mode_maps_severity_to_an_exit_code() {
        let critical = vec![probe::Finding::new("a", Severity::Critical, "t")];
        let warning = vec![probe::Finding::new("b", Severity::Warning, "t")];
        let note = vec![probe::Finding::new("c", Severity::Note, "t")];

        assert_eq!(exit_code(&critical, true), 2);
        assert_eq!(exit_code(&warning, true), 1);
        assert_eq!(exit_code(&note, true), 0);
        assert_eq!(exit_code(&[], true), 0);
    }

    #[test]
    fn without_strict_mode_doctor_never_fails_a_script() {
        let critical = vec![probe::Finding::new("a", Severity::Critical, "t")];
        assert_eq!(exit_code(&critical, false), 0);
    }

    #[test]
    fn a_default_config_skips_nothing() {
        assert!(skipped_areas(&Config::default(), &[]).is_empty());
    }

    #[test]
    fn switching_an_area_off_in_config_is_reported_as_skipped() {
        let mut cfg = Config::default();
        cfg.context.logs = false;
        cfg.context.network = false;
        let skipped = skipped_areas(&cfg, &[]);
        assert!(skipped.contains(&"logs"));
        assert!(skipped.contains(&"network"));
        assert_eq!(skipped.len(), 2);
    }

    #[test]
    fn narrowing_to_one_area_reports_the_rest_as_skipped() {
        let cfg = Config::default();
        let skipped = skipped_areas(&cfg, &[probe::Area::Network]);
        assert_eq!(skipped.len(), 5);
        assert!(!skipped.contains(&"network"));
    }
}
