//! Rendering a report for a person.
//!
//! The shape is fixed: a one-line verdict, then findings worst-first, each
//! with what Oracle saw and what it would try. Evidence is not optional and is
//! not hidden behind a verbosity flag. A troubleshooting tool that says "your
//! disk is fine" without showing the number is asking to be trusted, and it
//! has not earned that.

use crate::probe::{Finding, Severity, SystemView};

/// Print the findings. An empty list prints nothing: the verdict line above
/// has already said there is nothing, and saying it twice reads like a bug.
pub fn print(findings: &[Finding], verbose: bool) {
    for (i, f) in findings.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!(
            "{} {}",
            f.severity.paint(&format!("{}:", f.severity.label())),
            crate::ui::bold(&f.title)
        );

        for e in &f.evidence {
            println!("  {}", crate::ui::dim(&wrap(e, 4)));
        }

        for s in &f.suggestions {
            println!("  {}", wrap(&s.what, 2));
            if let Some(cmd) = &s.command {
                let shown = if s.needs_root && !crate::sys::is_root() {
                    format!("sudo {cmd}")
                } else {
                    cmd.clone()
                };
                crate::ui::command(&shown);
            }
        }

        if verbose {
            println!("  {}", crate::ui::dim(&format!("id: {}", f.id)));
        }
    }
}

/// The single line that goes above a report.
pub fn verdict(findings: &[Finding]) -> String {
    let summary = crate::diagnose::summarise(findings);
    let worst = findings.iter().map(|f| f.severity).min();
    match worst {
        None => crate::ui::green(&summary),
        Some(s) => s.paint(&summary),
    }
}

/// Everything as JSON, for piping somewhere else.
///
/// This is also the honest way to show someone exactly what Oracle knows about
/// their machine: the same structure the model would be given, on their own
/// terminal, before anything is sent anywhere.
pub fn json(view: &SystemView, findings: &[Finding], redact: bool) -> String {
    let value = serde_json::json!({
        "oracle": env!("CARGO_PKG_VERSION"),
        "findings": findings,
        "system": view,
    });
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into());
    if redact {
        crate::redact::scrub(&text)
    } else {
        text
    }
}

/// Wrap text to a sensible width, indenting continuation lines.
///
/// Fixed at 92 columns rather than read from the terminal: a report that
/// reflows differently depending on the window cannot be compared against the
/// one someone pasted into a bug report.
fn wrap(text: &str, indent: usize) -> String {
    const WIDTH: usize = 92;
    let pad = " ".repeat(indent);
    let mut out = String::new();
    let mut col = indent;

    for word in text.split_whitespace() {
        let w = word.chars().count();
        if col > indent && col + 1 + w > WIDTH {
            out.push('\n');
            out.push_str(&pad);
            col = indent;
        } else if col > indent {
            out.push(' ');
            col += 1;
        }
        out.push_str(word);
        col += w;
    }
    out
}

/// Count findings at or above a severity, for an exit code.
pub fn count_at_least(findings: &[Finding], level: Severity) -> usize {
    findings.iter().filter(|f| f.severity <= level).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Suggestion;

    #[test]
    fn wrapping_indents_continuation_lines_and_keeps_every_word() {
        let long = "one two three four five six seven eight nine ten eleven twelve thirteen \
                    fourteen fifteen sixteen seventeen eighteen nineteen twenty twentyone";
        let wrapped = wrap(long, 4);
        assert!(wrapped.contains('\n'), "long text must wrap");
        for line in wrapped.lines().skip(1) {
            assert!(line.starts_with("    "), "got {line:?}");
        }
        let original: Vec<&str> = long.split_whitespace().collect();
        let after: Vec<&str> = wrapped.split_whitespace().collect();
        assert_eq!(original, after, "wrapping must not lose or reorder words");
    }

    #[test]
    fn short_text_is_left_on_one_line() {
        assert_eq!(wrap("short enough", 2), "short enough");
    }

    #[test]
    fn the_verdict_of_an_empty_report_says_nothing_is_wrong() {
        crate::ui::init(true);
        assert_eq!(verdict(&[]), "Nothing to report.");
    }

    #[test]
    fn counting_by_severity_includes_everything_worse() {
        let f = vec![
            Finding::new("a", Severity::Critical, "t"),
            Finding::new("b", Severity::Warning, "t"),
            Finding::new("c", Severity::Note, "t"),
        ];
        assert_eq!(count_at_least(&f, Severity::Critical), 1);
        assert_eq!(count_at_least(&f, Severity::Warning), 2);
        assert_eq!(count_at_least(&f, Severity::Note), 3);
    }

    #[test]
    fn json_output_is_valid_json_and_carries_the_findings() {
        let view = SystemView::default();
        let f = vec![
            Finding::new("storage.full", Severity::Critical, "/ is full")
                .evidence("1G free")
                .suggest(Suggestion::cmd("look", "du -sh /")),
        ];
        let text = json(&view, &f, true);
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("must be valid JSON");
        assert_eq!(parsed["findings"][0]["id"], "storage.full");
        assert_eq!(parsed["findings"][0]["severity"], "critical");
    }
}
