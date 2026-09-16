//! Argument parsing and help text.
//!
//! Hand-rolled rather than derived from a crate, for the same reason the HTTP
//! client is: it is a hundred lines, it keeps the dependency tree to three
//! pure-Rust crates, and it gives exact control over the help text -- which is
//! the part of a tool like this that people actually read.

use crate::probe::Area;
#[derive(Debug)]
pub enum Command {
    /// No arguments: say what Oracle is and stop.
    Overview,
    Doctor(DoctorArgs),
    Ask(AskArgs),
    Explain(ExplainArgs),
    Context {
        json: bool,
    },
    /// The full-screen terminal interface.
    Tui,
    Status,
    Setup,
    Voice(VoiceCommand),
    Listen {
        seconds: Option<u32>,
    },
    Dictate {
        seconds: Option<u32>,
    },
    Forget {
        assume_yes: bool,
    },
    Version,
    Help(Option<String>),
}

#[derive(Debug, Default)]
pub struct DoctorArgs {
    pub json: bool,
    /// Permit checks that emit network traffic.
    pub online: bool,
    /// Permit checks that are slow or spawn the package manager.
    pub slow: bool,
    /// Narrow the probes to these areas.
    pub areas: Vec<Area>,
    /// Ask the model to summarise the findings in plain language.
    pub explain: bool,
    pub verbose: bool,
    /// Exit non-zero when something was found.
    pub strict: bool,
}

#[derive(Debug, Default)]
pub struct AskArgs {
    pub question: String,
    /// Print the assembled prompt instead of sending it.
    pub dry_run: bool,
    pub areas: Vec<Area>,
    pub online: bool,
    /// Answer once and exit, rather than staying for follow-up questions.
    pub once: bool,
}

#[derive(Debug, Default)]
pub struct ExplainArgs {
    /// Empty means read stdin.
    pub text: String,
    pub dry_run: bool,
    /// Answer once and exit, rather than staying for follow-up questions.
    pub once: bool,
}

#[derive(Debug)]
pub enum VoiceCommand {
    Setup,
    Status,
    Models,
    Fetch { name: String, assume_yes: bool },
    Test,
}

#[derive(Debug, Default)]
pub struct Global {
    pub plain: bool,
    pub quiet: bool,
}

#[derive(Debug)]
pub struct Parsed {
    pub global: Global,
    pub command: Command,
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Parsed, String> {
    let mut args: Vec<String> = args.into_iter().collect();
    let mut global = Global::default();

    // Global flags are accepted anywhere, because people type them anywhere.
    args.retain(|a| match a.as_str() {
        "--plain" | "--no-color" | "--no-colour" => {
            global.plain = true;
            false
        }
        "--quiet" | "-q" => {
            global.quiet = true;
            false
        }
        _ => true,
    });

    let mut it = args.into_iter().peekable();
    let Some(first) = it.next() else {
        return Ok(Parsed {
            global,
            command: Command::Overview,
        });
    };

    let command = match first.as_str() {
        "help" | "--help" | "-h" => Command::Help(it.next()),
        "version" | "--version" | "-V" => Command::Version,
        "doctor" | "check" => Command::Doctor(parse_doctor(it)?),
        "ask" => Command::Ask(parse_ask(it)?),
        "explain" => Command::Explain(parse_explain(it)?),
        "context" => {
            let json = it.any(|a| a == "--json");
            Command::Context { json }
        }
        "tui" | "ui" => Command::Tui,
        "status" => Command::Status,
        "setup" => Command::Setup,
        "voice" => Command::Voice(parse_voice(it)?),
        "listen" => Command::Listen {
            seconds: parse_seconds(it)?,
        },
        "dictate" => Command::Dictate {
            seconds: parse_seconds(it)?,
        },
        "forget" => Command::Forget {
            assume_yes: it.any(|a| a == "--yes" || a == "-y"),
        },
        other if other.starts_with('-') => {
            return Err(format!("unknown option {other}. Try `oracle help`."));
        }
        // A bare question is the most natural thing to type, so treat it as
        // one rather than as a mistake: `oracle why is my wifi down`.
        other => {
            let mut words = vec![other.to_string()];
            words.extend(it);
            Command::Ask(AskArgs {
                question: words.join(" "),
                ..Default::default()
            })
        }
    };

    Ok(Parsed { global, command })
}

fn parse_doctor<I: Iterator<Item = String>>(it: I) -> Result<DoctorArgs, String> {
    let mut d = DoctorArgs::default();
    let mut it = it.peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => d.json = true,
            "--online" => d.online = true,
            "--slow" => d.slow = true,
            "--explain" => d.explain = true,
            "--verbose" | "-v" => d.verbose = true,
            "--strict" => d.strict = true,
            "--area" | "-a" => {
                let v = it.next().ok_or("--area needs a name")?;
                d.areas.push(parse_area(&v)?);
            }
            other if other.starts_with("--area=") => {
                d.areas.push(parse_area(&other[7..])?);
            }
            other => return Err(format!("unknown option {other} for `oracle doctor`")),
        }
    }
    Ok(d)
}

fn parse_ask<I: Iterator<Item = String>>(it: I) -> Result<AskArgs, String> {
    let mut a = AskArgs::default();
    let mut words: Vec<String> = Vec::new();
    let mut it = it.peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dry-run" => a.dry_run = true,
            "--online" => a.online = true,
            "--once" => a.once = true,
            "--area" => {
                let v = it.next().ok_or("--area needs a name")?;
                a.areas.push(parse_area(&v)?);
            }
            other if other.starts_with("--area=") => a.areas.push(parse_area(&other[7..])?),
            other => words.push(other.to_string()),
        }
    }
    a.question = words.join(" ").trim().to_string();
    if a.question.is_empty() {
        return Err("ask what? For example: oracle ask \"why is my wifi dropping\"".into());
    }
    Ok(a)
}

fn parse_explain<I: Iterator<Item = String>>(it: I) -> Result<ExplainArgs, String> {
    let mut e = ExplainArgs::default();
    let mut words: Vec<String> = Vec::new();
    for arg in it {
        match arg.as_str() {
            "--dry-run" => e.dry_run = true,
            "--once" => e.once = true,
            // `-` is the conventional way to say "read stdin".
            "-" => {}
            other => words.push(other.to_string()),
        }
    }
    e.text = words.join(" ");
    Ok(e)
}

fn parse_voice<I: Iterator<Item = String>>(it: I) -> Result<VoiceCommand, String> {
    let mut it = it.peekable();
    let sub = it.next().unwrap_or_else(|| "status".into());
    Ok(match sub.as_str() {
        "setup" => VoiceCommand::Setup,
        "status" => VoiceCommand::Status,
        "models" | "list" => VoiceCommand::Models,
        "test" => VoiceCommand::Test,
        "fetch" | "get" | "download" => {
            let rest: Vec<String> = it.collect();
            let assume_yes = rest.iter().any(|a| a == "--yes" || a == "-y");
            let name = rest
                .into_iter()
                .find(|a| !a.starts_with('-'))
                .ok_or("which model? Try `oracle voice models` for the list.")?;
            VoiceCommand::Fetch { name, assume_yes }
        }
        other => {
            return Err(format!(
                "unknown voice command {other}. Try: setup, status, models, fetch, test"
            ));
        }
    })
}

fn parse_seconds<I: Iterator<Item = String>>(it: I) -> Result<Option<u32>, String> {
    let mut seconds = None;
    let mut it = it.peekable();
    while let Some(a) = it.next() {
        let raw = match a.as_str() {
            "--seconds" | "-s" => it.next().ok_or("--seconds needs a number")?,
            other if other.starts_with("--seconds=") => other["--seconds=".len()..].to_string(),
            other => return Err(format!("unknown option {other}")),
        };
        seconds = Some(
            raw.parse::<u32>()
                .map_err(|_| format!("{raw} is not a number of seconds"))?,
        );
    }
    Ok(seconds)
}

fn parse_area(s: &str) -> Result<Area, String> {
    Area::parse(s).ok_or_else(|| {
        format!(
            "unknown area {s:?}. Known areas: {}",
            Area::all()
                .iter()
                .map(|a| a.name())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

pub const OVERVIEW: &str = "\
Oracle is a local troubleshooting helper. It reads your system, tells you what
looks wrong, and suggests what to try. It runs only when you run it.

  oracle tui                 the full-screen terminal interface
  raven-oracle               the desktop app, also in the launcher
  oracle doctor              check this machine and report
  oracle ask \"...\"           ask a question about this machine
  oracle explain             explain an error you paste or pipe in
  oracle status              what Oracle is set up to do
  oracle setup               configure it (optional)

It does not change anything, run in the background, or start at login.
`oracle help` has the rest.
";

pub const HELP: &str = "\
oracle -- a local troubleshooting companion

USAGE
  oracle <command> [options]
  oracle <question>                 shorthand for `oracle ask`

COMMANDS
  tui                               the full-screen terminal interface
  doctor                            check the machine, report what looks wrong
  ask <question>                    ask about this machine, in plain language
  explain [text]                    explain an error; reads stdin if piped
  context                           show exactly what Oracle would send a model
  status                            what is configured and what is missing
  setup                             configure Oracle (writes one file)
  voice <sub>                       dictation: setup, status, models, fetch, test
  listen                            dictate a question, confirm it, then ask
  dictate                           transcribe speech to stdout and nothing else
  forget                            delete Oracle's config and downloaded models
  version                           print the version

DOCTOR OPTIONS
  --area <name>                     limit to: services, storage, network,
                                    packages, logs, hardware. Repeatable.
  --explain                         have the model summarise the findings
  --online                          permit checks that use the network
  --slow                            permit slow checks (pending updates)
  --json                            machine-readable output
  --strict                          exit non-zero when something is found
  -v, --verbose                     include finding identifiers

ASK OPTIONS
  --area <name>                     limit what gets read. Repeatable.
  --online                          permit checks that use the network
  --once                            answer once instead of staying for
                                    follow-up questions
  --dry-run                         print the prompt instead of sending it

FOLLOW-UP QUESTIONS
  `ask` and `explain` stay open for follow-ups when they are run in a
  terminal, because one answer is rarely the end of it. Press Enter on an
  empty line, or Ctrl-D, to leave. The machine is read again before each
  answer, so 'did that fix it?' is a question Oracle can actually see the
  answer to. Piped or redirected, both answer once and exit, and `--once`
  says so explicitly.

GLOBAL OPTIONS
  --plain                           no colour, no decoration
  -q, --quiet                       results only

WHAT IT DOES NOT DO
  No daemon, no autostart, no notifications, no telemetry.
  It never runs a command for you. It prints commands and you decide.
  The model endpoint must be on this machine, and nothing is sent anywhere
  else. `oracle context` prints exactly what would be sent, before it is.

FILES
  ~/.config/raven/oracle.toml       written only by `oracle setup`
  ~/.local/share/raven/oracle/      whisper models, if you fetch any

Nothing else on the system reads these. Remove the binary and Oracle is gone.
";

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Parsed, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_arguments_gives_the_overview_rather_than_starting_anything() {
        // A bare `oracle` must not open a full-screen interface. Taking over
        // somebody's terminal because they typed the name is the most
        // intrusive thing this program could do.
        assert!(matches!(p(&[]).unwrap().command, Command::Overview));
    }

    #[test]
    fn the_interface_opens_only_when_it_is_named() {
        assert!(matches!(p(&["tui"]).unwrap().command, Command::Tui));
        assert!(matches!(p(&["ui"]).unwrap().command, Command::Tui));
    }

    #[test]
    fn a_bare_question_is_treated_as_a_question() {
        let parsed = p(&["why", "is", "my", "wifi", "down"]).unwrap();
        match parsed.command {
            Command::Ask(a) => assert_eq!(a.question, "why is my wifi down"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_question_beginning_with_why_keeps_the_word_why() {
        // `why` was once a subcommand alias, which silently ate the first word
        // of the most natural thing anyone types.
        let parsed = p(&["why", "does", "the", "store", "hang"]).unwrap();
        match parsed.command {
            Command::Ask(a) => assert_eq!(a.question, "why does the store hang"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn each_promise_in_the_help_text_survives_on_one_line() {
        // These lines are the product's contract with the user. A promise
        // broken across a line cannot be quoted or grepped whole.
        for promise in [
            "It never runs a command for you.",
            "No daemon, no autostart, no notifications, no telemetry.",
        ] {
            assert!(
                HELP.lines().any(|l| l.contains(promise)),
                "{promise:?} is not intact on one line of the help text"
            );
        }
    }

    #[test]
    fn a_question_stays_for_follow_ups_unless_told_otherwise() {
        let Command::Ask(a) = p(&["ask", "why is it slow"]).unwrap().command else {
            panic!("expected ask");
        };
        assert!(
            !a.once,
            "a question in a terminal is the start of a conversation"
        );

        let Command::Ask(a) = p(&["ask", "--once", "why is it slow"]).unwrap().command else {
            panic!("expected ask");
        };
        assert!(a.once);
        assert_eq!(
            a.question, "why is it slow",
            "--once is not part of the question"
        );
    }

    #[test]
    fn explain_takes_once_too() {
        let Command::Explain(e) = p(&["explain", "--once", "error: nope"]).unwrap().command else {
            panic!("expected explain");
        };
        assert!(e.once);
        assert_eq!(e.text, "error: nope");
    }

    #[test]
    fn ask_with_no_question_explains_itself() {
        let e = p(&["ask"]).unwrap_err();
        assert!(e.contains("ask what?"), "got {e}");
    }

    #[test]
    fn global_flags_work_wherever_they_are_typed() {
        let parsed = p(&["doctor", "--plain", "--json"]).unwrap();
        assert!(parsed.global.plain);
        match parsed.command {
            Command::Doctor(d) => assert!(d.json),
            other => panic!("got {other:?}"),
        }

        let parsed = p(&["--quiet", "doctor"]).unwrap();
        assert!(parsed.global.quiet);
    }

    #[test]
    fn areas_can_be_given_either_way_and_repeated() {
        let parsed = p(&["doctor", "--area", "network", "--area=storage"]).unwrap();
        match parsed.command {
            Command::Doctor(d) => {
                assert_eq!(d.areas, vec![Area::Network, Area::Storage]);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_area_lists_the_real_ones() {
        let e = p(&["doctor", "--area", "quantum"]).unwrap_err();
        assert!(e.contains("network"), "got {e}");
        assert!(e.contains("storage"), "got {e}");
    }

    #[test]
    fn online_and_slow_checks_are_off_unless_asked_for() {
        match p(&["doctor"]).unwrap().command {
            Command::Doctor(d) => {
                assert!(!d.online, "a quiet tool does not emit traffic by default");
                assert!(!d.slow);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn voice_defaults_to_status_rather_than_to_recording() {
        assert!(matches!(
            p(&["voice"]).unwrap().command,
            Command::Voice(VoiceCommand::Status)
        ));
    }

    #[test]
    fn fetching_a_model_needs_a_name() {
        assert!(p(&["voice", "fetch"]).is_err());
        match p(&["voice", "fetch", "base.en"]).unwrap().command {
            Command::Voice(VoiceCommand::Fetch { name, assume_yes }) => {
                assert_eq!(name, "base.en");
                assert!(!assume_yes);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn seconds_can_be_given_either_way() {
        match p(&["dictate", "--seconds", "10"]).unwrap().command {
            Command::Dictate { seconds } => assert_eq!(seconds, Some(10)),
            other => panic!("got {other:?}"),
        }
        match p(&["listen", "--seconds=5"]).unwrap().command {
            Command::Listen { seconds } => assert_eq!(seconds, Some(5)),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_repeated_seconds_flag_takes_the_last_one() {
        // The parser was once a loop whose body always returned, so only the
        // first argument was ever looked at.
        match p(&["dictate", "--seconds", "10", "--seconds=30"])
            .unwrap()
            .command
        {
            Command::Dictate { seconds } => assert_eq!(seconds, Some(30)),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_seconds_value_that_is_not_a_number_says_so() {
        let e = p(&["dictate", "--seconds", "soon"]).unwrap_err();
        assert!(e.contains("not a number"), "got {e}");
    }

    #[test]
    fn an_unknown_option_is_a_clear_error_not_a_question() {
        let e = p(&["--frobnicate"]).unwrap_err();
        assert!(e.contains("unknown option"), "got {e}");
    }

    #[test]
    fn the_help_text_states_what_oracle_will_not_do() {
        assert!(HELP.contains("No daemon"));
        assert!(HELP.contains("no autostart"));
        assert!(HELP.contains("It never runs a command for you."));
        assert!(OVERVIEW.contains("only when you run it"));
    }
}
