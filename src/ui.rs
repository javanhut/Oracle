//! Terminal output.
//!
//! Oracle is a guest on someone's terminal. It writes plainly, colours only
//! when a human is actually looking at a TTY, and never clears the screen,
//! moves the cursor, or draws anything that would still be on screen after it
//! exits. Piping `oracle` into a file gives you the same text without escapes.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static COLOUR: AtomicBool = AtomicBool::new(false);
static QUIET: AtomicBool = AtomicBool::new(false);

/// Decide once, at startup, whether this run is allowed to emit colour.
///
/// `NO_COLOR` (any value) and a non-TTY stdout both turn it off, which is the
/// behaviour every other tool in the Raven layer follows.
pub fn init(force_plain: bool) {
    let enabled = !force_plain
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
        && std::io::stdout().is_terminal();
    COLOUR.store(enabled, Ordering::Relaxed);
}

pub fn set_quiet(q: bool) {
    QUIET.store(q, Ordering::Relaxed);
}

pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

fn colour() -> bool {
    COLOUR.load(Ordering::Relaxed)
}

/// Wrap `s` in an SGR sequence, or return it untouched when colour is off.
fn paint(s: &str, code: &str) -> String {
    if colour() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint(s, "1")
}
pub fn dim(s: &str) -> String {
    paint(s, "2")
}
pub fn red(s: &str) -> String {
    paint(s, "31")
}
pub fn green(s: &str) -> String {
    paint(s, "32")
}
pub fn yellow(s: &str) -> String {
    paint(s, "33")
}
pub fn blue(s: &str) -> String {
    paint(s, "34")
}
pub fn cyan(s: &str) -> String {
    paint(s, "36")
}

/// A section heading. Blank line above, never below, so sections stack without
/// accumulating whitespace.
pub fn heading(s: &str) {
    if is_quiet() {
        return;
    }
    println!("\n{}", bold(s));
}

pub fn info(s: &str) {
    if is_quiet() {
        return;
    }
    println!("{s}");
}

/// A note that is not part of the answer: progress, context, reassurance.
/// Goes to stderr so that `oracle ask ... > answer.txt` captures only the
/// answer.
pub fn note(s: &str) {
    if is_quiet() {
        return;
    }
    eprintln!("{}", dim(s));
}

pub fn warn(s: &str) {
    eprintln!("{} {s}", yellow("warning:"));
}

pub fn error(s: &str) {
    eprintln!("{} {s}", red("error:"));
}

/// A suggested shell command. Indented and marked so it is obvious that Oracle
/// is showing it rather than running it, and so it can be copied cleanly.
pub fn command(cmd: &str) {
    for line in cmd.lines() {
        println!("    {}", cyan(line));
    }
}

/// Ask a yes/no question. Defaults to no, and answers no without asking when
/// stdin is not a terminal -- an unattended run must never block waiting for a
/// keystroke that will never come.
pub fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Read a line of free text, returning `None` at EOF or on a non-TTY stdin.
pub fn prompt_line(question: &str) -> Option<String> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()),
    }
}

/// Render bytes as a human-sized string. Used everywhere a probe reports a
/// size, so the units stay consistent across the whole report.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else if v >= 100.0 {
        format!("{v:.0}{}", UNITS[i])
    } else {
        format!("{v:.1}{}", UNITS[i])
    }
}
