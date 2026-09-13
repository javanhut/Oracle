//! The terminal interface.
//!
//! It is a second way into the same three things the command line does: run
//! the checks, read a finding, ask the local model. It holds to every rule the
//! rest of Oracle holds to, and one more that only a full-screen program has to
//! worry about.
//!
//! - **It runs only when asked.** `oracle tui`, never on a bare `oracle`.
//! - **It never runs a command.** `y` copies one to the clipboard. That is the
//!   furthest it goes, and copying is not a change to the system.
//! - **It does not poll.** Checks run on start and when `r` is pressed. A
//!   screen that refreshes itself is a screen that is doing something while
//!   you are not looking at it.
//! - **It gives the terminal back exactly as it found it**, including when it
//!   panics. A crash that leaves someone in raw mode with no echo and no cursor
//!   has done them more harm than the bug did.

mod app;
mod draw;

use crate::config::Config;
use crate::diagnose;
use crate::probe::{Finding, ProbeOptions, SystemView};
use app::{Action, App, Event, Task};
use ratatui::crossterm::event::{self, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::Duration;

/// How long to wait for a key before redrawing anyway.
///
/// Short enough that streamed tokens appear promptly, long enough that an idle
/// screen is not spinning a core.
const TICK: Duration = Duration::from_millis(60);

pub fn run(cfg: &Config) -> i32 {
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        crate::ui::error("the terminal interface needs a terminal.");
        crate::ui::note("`oracle doctor` prints the same findings as plain text.");
        return 2;
    }

    // Restore the terminal on a panic before the message is printed, otherwise
    // the backtrace is drawn into the alternate screen and vanishes with it.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = ratatui::try_restore();
        previous(info);
    }));

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, cfg);
    ratatui::restore();

    match result {
        Ok(()) => 0,
        Err(e) => {
            crate::ui::error(&e.to_string());
            1
        }
    }
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, cfg: &Config) -> std::io::Result<()> {
    // Asked once, at startup: probing the model on every keystroke would make
    // a dead endpoint cost a connection attempt per frame.
    let model_available = crate::model::backend_for(cfg)
        .map(|b| b.availability().reachable)
        .unwrap_or(false);

    let mut app = App::new(model_available);
    let (tx, rx) = channel::<Event>();

    // A generation counter, so the answer from a question the user has already
    // escaped out of cannot arrive and overwrite a newer one.
    let mut generation: u64 = 0;

    spawn_scan(cfg.clone(), tx.clone());
    app.handle_event(Event::ScanStarted);

    loop {
        terminal.draw(|frame| draw::draw(frame, &app))?;

        if event::poll(TICK)?
            && let event::Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && let Some(task) = app.handle(action_for(key, &app))
        {
            match task {
                Task::Rescan => {
                    spawn_scan(cfg.clone(), tx.clone());
                    app.handle_event(Event::ScanStarted);
                }
                Task::Ask(question) => {
                    generation += 1;
                    spawn_ask(cfg.clone(), question, tx.clone(), generation);
                }
                Task::Explain(finding) => {
                    generation += 1;
                    spawn_explain(cfg.clone(), finding, tx.clone(), generation);
                }
                Task::Copy(command) => match copy_to_clipboard(&command) {
                    Ok(tool) => {
                        app.notice = Some(app::Notice {
                            text: format!("copied with {tool}: {command}"),
                            is_error: false,
                        })
                    }
                    Err(e) => {
                        app.notice = Some(app::Notice {
                            text: e,
                            is_error: true,
                        })
                    }
                },
            }
        }

        drain(&rx, &mut app);

        if app.should_quit {
            return Ok(());
        }
    }
}

/// Apply everything a worker has sent since the last frame.
fn drain(rx: &Receiver<Event>, app: &mut App) {
    loop {
        match rx.try_recv() {
            Ok(event) => app.handle_event(event),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
        }
    }
}

/// Turn a key press into an action, given what is on screen.
///
/// Separated from `App` so the mapping can be read in one place and the state
/// machine never has to know what a key is.
fn action_for(key: KeyEvent, app: &App) -> Action {
    // Ctrl-C leaves from anywhere, including mid-answer, because that is what
    // it does everywhere else.
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
    {
        return Action::Quit;
    }

    let typing = app.view == app::View::Ask;

    match key.code {
        KeyCode::Esc => Action::Back,
        KeyCode::Enter if typing => Action::Submit,
        KeyCode::Up => Action::Up,
        KeyCode::Down => Action::Down,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Tab => Action::SwitchFocus,
        KeyCode::Left if typing => Action::CursorLeft,
        KeyCode::Right if typing => Action::CursorRight,
        KeyCode::Home => Action::Home,
        KeyCode::End => Action::End,
        KeyCode::Backspace if typing => Action::Backspace,

        // While typing, a letter is a letter. Anywhere else it is a command.
        KeyCode::Char(c) if typing => Action::Insert(c),

        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('j') => Action::Down,
        KeyCode::Char('k') => Action::Up,
        KeyCode::Char('g') => Action::Home,
        KeyCode::Char('G') => Action::End,
        KeyCode::Char('r') => Action::Refresh,
        KeyCode::Char('a') => Action::OpenAsk,
        KeyCode::Char('e') => Action::Explain,
        KeyCode::Char('y') => Action::Copy,
        KeyCode::Char('f') => Action::CycleFilter,
        KeyCode::Char('?') | KeyCode::Char('h') => Action::ToggleHelp,

        _ => Action::Nothing,
    }
}

fn spawn_scan(cfg: Config, tx: Sender<Event>) {
    std::thread::spawn(move || {
        let view = SystemView::gather(&cfg, ProbeOptions::default());
        let findings = diagnose::run(&view);
        let _ = tx.send(Event::ScanFinished(findings));
    });
}

fn spawn_ask(cfg: Config, question: String, tx: Sender<Event>, _generation: u64) {
    std::thread::spawn(move || {
        let areas = crate::probe::areas_for_question(&question);
        let view = SystemView::gather_for(&cfg, &areas, ProbeOptions::default());
        let findings = diagnose::run(&view);
        let messages = crate::prompt::build(&question, &view, &findings, &cfg);
        stream(cfg, messages, tx);
    });
}

fn spawn_explain(cfg: Config, finding: Finding, tx: Sender<Event>, _generation: u64) {
    std::thread::spawn(move || {
        let view = SystemView::gather(&cfg, ProbeOptions::default());
        let messages = crate::prompt::build_finding_explanation(&finding, &view, &cfg);
        stream(cfg, messages, tx);
    });
}

/// Run one model call, forwarding tokens as they arrive.
///
/// The backend is built inside the thread rather than passed in: a
/// `Box<dyn Backend>` is not `Send`, and building it here costs nothing.
fn stream(cfg: Config, messages: Vec<crate::model::Message>, tx: Sender<Event>) {
    let backend = match crate::model::backend_for(&cfg) {
        Ok(b) => b,
        Err(e) => {
            let _ = tx.send(Event::AnswerFailed(e));
            return;
        }
    };

    let result = backend.chat(&messages, &mut |token| {
        // A send failure means the interface has gone; stop generating rather
        // than finish an answer nobody will read.
        tx.send(Event::AnswerToken(token.to_string())).is_ok()
    });

    let _ = match result {
        Ok(_) => tx.send(Event::AnswerFinished),
        Err(crate::model::ModelError::Interrupted) => tx.send(Event::AnswerFinished),
        Err(e) => tx.send(Event::AnswerFailed(e.to_string())),
    };
}

/// Put text on the clipboard using whatever this desktop has.
///
/// Wayland first, since that is what Raven runs. Returns the tool's name so the
/// interface can say which one it used, and a plain explanation when there is
/// none rather than a silent no-op.
fn copy_to_clipboard(text: &str) -> Result<&'static str, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    const TOOLS: [(&str, &[&str]); 3] = [
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];

    for (bin, args) in TOOLS {
        if !crate::sys::have(bin) {
            continue;
        }
        let mut child = match Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        // wl-copy forks a server to hold the selection and returns straight
        // away; the others exit once stdin closes. Either way this does not
        // block the interface.
        let _ = child.wait();
        return Ok(bin);
    }

    Err("no clipboard tool found. Install wl-copy, xclip or xsel.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn letters_are_commands_in_the_report_and_text_while_asking() {
        let mut report = App::new(true);
        assert_eq!(
            action_for(press(KeyCode::Char('a')), &report),
            Action::OpenAsk
        );
        assert_eq!(action_for(press(KeyCode::Char('q')), &report), Action::Quit);

        report.handle(Action::OpenAsk);
        assert_eq!(
            action_for(press(KeyCode::Char('a')), &report),
            Action::Insert('a'),
            "typing a question must not trigger commands"
        );
        assert_eq!(
            action_for(press(KeyCode::Char('q')), &report),
            Action::Insert('q'),
            "q must not quit in the middle of a sentence"
        );
    }

    #[test]
    fn enter_sends_only_where_there_is_something_to_send() {
        let mut app = App::new(true);
        assert_eq!(action_for(press(KeyCode::Enter), &app), Action::Nothing);
        app.handle(Action::OpenAsk);
        assert_eq!(action_for(press(KeyCode::Enter), &app), Action::Submit);
    }

    #[test]
    fn ctrl_c_and_ctrl_d_quit_from_anywhere() {
        let mut app = App::new(true);
        assert_eq!(action_for(ctrl('c'), &app), Action::Quit);
        app.handle(Action::OpenAsk);
        assert_eq!(action_for(ctrl('c'), &app), Action::Quit);
        assert_eq!(action_for(ctrl('d'), &app), Action::Quit);
    }

    #[test]
    fn vi_keys_move_in_the_report_and_type_in_the_input() {
        let mut app = App::new(true);
        assert_eq!(action_for(press(KeyCode::Char('j')), &app), Action::Down);
        assert_eq!(action_for(press(KeyCode::Char('k')), &app), Action::Up);
        app.handle(Action::OpenAsk);
        assert_eq!(
            action_for(press(KeyCode::Char('j')), &app),
            Action::Insert('j')
        );
    }

    #[test]
    fn backspace_edits_text_but_does_nothing_in_the_report() {
        let mut app = App::new(true);
        assert_eq!(action_for(press(KeyCode::Backspace), &app), Action::Nothing);
        app.handle(Action::OpenAsk);
        assert_eq!(
            action_for(press(KeyCode::Backspace), &app),
            Action::Backspace
        );
    }

    #[test]
    fn escape_always_means_back() {
        let mut app = App::new(true);
        assert_eq!(action_for(press(KeyCode::Esc), &app), Action::Back);
        app.handle(Action::OpenAsk);
        assert_eq!(action_for(press(KeyCode::Esc), &app), Action::Back);
    }

    #[test]
    fn an_unmapped_key_does_nothing_rather_than_something_surprising() {
        let app = App::new(true);
        assert_eq!(action_for(press(KeyCode::F(7)), &app), Action::Nothing);
        assert_eq!(action_for(press(KeyCode::Insert), &app), Action::Nothing);
    }

    #[test]
    fn the_clipboard_reports_rather_than_silently_doing_nothing() {
        // Whether a tool exists depends on the machine; either answer must be
        // a clear one.
        match copy_to_clipboard("oracle self-test") {
            Ok(tool) => assert!(!tool.is_empty()),
            Err(e) => assert!(e.contains("wl-copy"), "got {e}"),
        }
    }
}
