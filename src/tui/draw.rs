//! Drawing. Reads `App`, writes nothing back.

use super::app::{Answer, App, Filter, Focus, View};
use crate::probe::{Finding, Severity};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};

/// Severity colours, matching what the command line prints, so a finding looks
/// the same whichever way somebody is reading it.
fn severity_style(s: Severity) -> Style {
    match s {
        Severity::Critical => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Severity::Warning => Style::default().fg(Color::Yellow),
        Severity::Note => Style::default().fg(Color::Blue),
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Min(3),    // body
        Constraint::Length(1), // status
    ])
    .split(frame.area());

    title(frame, areas[0], app);

    match app.view {
        View::Report => report(frame, areas[1], app),
        View::Ask => ask(frame, areas[1], app),
        View::Help => {
            report(frame, areas[1], app);
            help_overlay(frame, areas[1]);
        }
    }

    status(frame, areas[2], app);
}

fn title(frame: &mut Frame, area: Rect, app: &App) {
    let (critical, warning, note) = app.counts();

    let mut spans = vec![
        Span::styled(" oracle ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("· read-only ", Style::default().fg(Color::DarkGray)),
    ];

    if app.scanning {
        spans.push(Span::styled(
            "· checking…",
            Style::default().fg(Color::Cyan),
        ));
    } else if !app.scanned_once {
        spans.push(Span::styled(
            "· starting",
            Style::default().fg(Color::DarkGray),
        ));
    } else if critical + warning + note == 0 {
        spans.push(Span::styled(
            "· nothing to report",
            Style::default().fg(Color::Green),
        ));
    } else {
        spans.push(Span::raw("· "));
        for (n, label, sev) in [
            (critical, "critical", Severity::Critical),
            (warning, "warning", Severity::Warning),
            (note, "note", Severity::Note),
        ] {
            if n > 0 {
                spans.push(Span::styled(
                    format!("{n} {label}{} ", if n == 1 { "" } else { "s" }),
                    severity_style(sev),
                ));
            }
        }
    }

    if app.filter != Filter::All {
        spans.push(Span::styled(
            format!("· {} ", app.filter.label()),
            Style::default().fg(Color::Magenta),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn report(frame: &mut Frame, area: Rect, app: &App) {
    let panes =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).split(area);

    findings_list(frame, panes[0], app);
    detail(frame, panes[1], app);
}

fn pane_block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().add_modifier(Modifier::BOLD),
        ))
}

fn findings_list(frame: &mut Frame, area: Rect, app: &App) {
    let visible = app.visible();
    let block = pane_block("findings", app.focus == Focus::List);

    if visible.is_empty() {
        let msg = if !app.scanned_once {
            "Checking this machine…"
        } else if app.findings.is_empty() {
            "Nothing to report.\n\nPress r to check again, a to ask a question."
        } else {
            "Nothing matches this filter.\n\nPress f to widen it."
        };
        frame.render_widget(
            Paragraph::new(msg)
                .block(block)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = visible
        .iter()
        .map(|f| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<9}", f.severity.label()),
                    severity_style(f.severity),
                ),
                Span::raw(f.title.clone()),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.selected));

    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▍"),
        area,
        &mut state,
    );
}

fn detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = pane_block("detail", app.focus == Focus::Detail);

    let Some(f) = app.selected_finding() else {
        frame.render_widget(Paragraph::new("").block(block), area);
        return;
    };

    frame.render_widget(
        Paragraph::new(detail_lines(f))
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        area,
    );
}

/// The body of a finding, laid out the same way the printed report lays it out.
pub fn detail_lines(f: &Finding) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            f.title.clone(),
            severity_style(f.severity).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];

    if !f.evidence.is_empty() {
        lines.push(Line::from(Span::styled(
            "What I saw",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for e in &f.evidence {
            lines.push(Line::from(vec![
                Span::styled("  · ", Style::default().fg(Color::DarkGray)),
                Span::raw(e.clone()),
            ]));
        }
        lines.push(Line::raw(""));
    }

    if !f.suggestions.is_empty() {
        lines.push(Line::from(Span::styled(
            "What I would try",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for s in &f.suggestions {
            lines.push(Line::from(vec![
                Span::styled("  · ", Style::default().fg(Color::DarkGray)),
                Span::raw(s.what.clone()),
            ]));
            if let Some(cmd) = &s.command {
                let shown = if s.needs_root && !crate::sys::is_root() {
                    format!("sudo {cmd}")
                } else {
                    cmd.clone()
                };
                lines.push(Line::from(Span::styled(
                    format!("      {shown}"),
                    Style::default().fg(Color::Cyan),
                )));
            }
        }
        lines.push(Line::raw(""));
    }

    // Stated on every finding rather than once in the help, because this is
    // the promise the whole tool rests on and the detail pane is where someone
    // is deciding whether to trust it.
    lines.push(Line::from(Span::styled(
        "Oracle does not run these. Press y to copy the first one.",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        format!("id: {}", f.id),
        Style::default().fg(Color::DarkGray),
    )));

    lines
}

fn ask(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).split(area);

    let mut lines: Vec<Line> = Vec::new();
    if !app.question.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("? ", Style::default().fg(Color::Cyan)),
            Span::styled(
                app.question.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::raw(""));
    }

    match &app.answer_state {
        Answer::Idle => lines.push(Line::from(Span::styled(
            "Type a question about this machine and press Enter.\n",
            Style::default().fg(Color::DarkGray),
        ))),
        Answer::Working if app.answer.is_empty() => lines.push(Line::from(Span::styled(
            "Reading the system, then asking the local model…",
            Style::default().fg(Color::DarkGray),
        ))),
        Answer::Failed(e) => {
            lines.push(Line::from(Span::styled(
                e.clone(),
                Style::default().fg(Color::Red),
            )));
        }
        _ => {}
    }

    for line in app.answer.lines() {
        lines.push(Line::raw(line.to_string()));
    }

    if app.answer_state == Answer::Streaming {
        lines.push(Line::from(Span::styled(
            "▌",
            Style::default().fg(Color::Cyan),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(pane_block("answer", false))
            .wrap(Wrap { trim: false })
            .scroll((app.answer_scroll, 0)),
        rows[0],
    );

    let input = Paragraph::new(Line::from(vec![
        Span::styled("› ", Style::default().fg(Color::Cyan)),
        Span::raw(app.input.clone()),
    ]))
    .block(pane_block("ask", true));
    frame.render_widget(input, rows[1]);

    // A real cursor, so typing feels like typing.
    let x = rows[1].x + 3 + app.cursor as u16;
    frame.set_cursor_position((x.min(rows[1].right().saturating_sub(2)), rows[1].y + 1));
}

fn status(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(n) = &app.notice {
        let style = if n.is_error {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {}", n.text), style))),
            area,
        );
        return;
    }

    let keys = match app.view {
        View::Report => {
            "  ↑↓ move · tab pane · y copy command · e explain · a ask · f filter · r recheck · ? help · q quit"
        }
        View::Ask => "  enter send · esc stop, again to go back · ↑↓ scroll · ? help",
        View::Help => "  any key closes this",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            keys,
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

fn help_overlay(frame: &mut Frame, area: Rect) {
    let width = 62.min(area.width.saturating_sub(4));
    let height = 22.min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);

    let lines = vec![
        Line::from(Span::styled("Moving about", bold)),
        Line::raw("  ↑ ↓ j k     move the selection"),
        Line::raw("  tab         swap between the two panes"),
        Line::raw("  pgup pgdn   jump, or scroll the detail"),
        Line::raw("  f           cycle the severity filter"),
        Line::raw(""),
        Line::from(Span::styled("Doing something", bold)),
        Line::raw("  r           check the machine again"),
        Line::raw("  a           ask a question"),
        Line::raw("  e           have the model explain this finding"),
        Line::raw("  y           copy this finding's command"),
        Line::raw(""),
        Line::from(Span::styled("Leaving", bold)),
        Line::raw("  esc         back, or stop a streaming answer"),
        Line::raw("  q           quit"),
        Line::raw(""),
        Line::from(Span::styled("Oracle never runs a command for you.", dim)),
        Line::from(Span::styled("Nothing here changes the system.", dim)),
    ];

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(" keys "),
            )
            .wrap(Wrap { trim: true }),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::super::app::{Action, Answer, App, Event};
    use super::*;
    use crate::probe::Suggestion;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Draw one frame and return everything visible as plain text.
    ///
    /// Rendering is the half of a TUI a state-machine test cannot reach: a
    /// layout that panics on a narrow terminal, or a pane that silently draws
    /// nothing, both pass every test in `app`.
    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app_with_findings() -> App {
        let mut app = App::new(true);
        app.handle_event(Event::ScanFinished(vec![
            Finding::new("storage.full", Severity::Critical, "/ is 99% full")
                .evidence("1G free of 100G")
                .suggest(Suggestion::cmd("see where it went", "du -sh /")),
            Finding::new(
                "network.no-dns",
                Severity::Warning,
                "No nameserver configured",
            )
            .evidence("/etc/resolv.conf lists no nameserver"),
        ]));
        app
    }

    #[test]
    fn the_report_draws_the_findings_and_the_selected_detail() {
        let screen = render(&app_with_findings(), 110, 30);
        assert!(screen.contains("critical"), "{screen}");
        assert!(screen.contains("/ is 99% full"));
        assert!(screen.contains("No nameserver configured"));
        // The first finding is selected, so its detail is on the right.
        assert!(screen.contains("1G free of 100G"));
        assert!(screen.contains("du -sh /"));
    }

    #[test]
    fn the_title_bar_counts_findings_by_severity() {
        let screen = render(&app_with_findings(), 110, 30);
        let title = screen.lines().next().unwrap();
        assert!(title.contains("1 critical"), "got {title}");
        assert!(title.contains("1 warning"), "got {title}");
    }

    #[test]
    fn moving_the_selection_changes_which_detail_is_drawn() {
        let mut app = app_with_findings();
        app.handle(Action::Down);
        let screen = render(&app, 110, 30);
        assert!(screen.contains("lists no nameserver"), "{screen}");
        assert!(!screen.contains("du -sh /"), "the old detail must be gone");
    }

    #[test]
    fn a_machine_with_nothing_wrong_says_so_rather_than_showing_an_empty_box() {
        let mut app = App::new(true);
        app.handle_event(Event::ScanFinished(vec![]));
        let screen = render(&app, 100, 24);
        assert!(screen.contains("Nothing to report"), "{screen}");
    }

    #[test]
    fn before_the_first_scan_it_says_it_is_working_not_that_all_is_well() {
        let app = App::new(true);
        let screen = render(&app, 100, 24);
        assert!(screen.contains("Checking this machine"), "{screen}");
        assert!(
            !screen.contains("Nothing to report"),
            "claiming a clean machine before looking is a lie"
        );
    }

    #[test]
    fn a_filter_that_hides_everything_explains_how_to_widen_it() {
        let mut app = app_with_findings();
        app.handle(Action::CycleFilter);
        app.handle(Action::CycleFilter);
        app.handle_event(Event::ScanFinished(vec![Finding::new(
            "x",
            Severity::Note,
            "just a note",
        )]));
        let screen = render(&app, 100, 24);
        assert!(screen.contains("Nothing matches this filter"), "{screen}");
        assert!(
            screen.contains("press f") || screen.contains("Press f"),
            "{screen}"
        );
    }

    #[test]
    fn the_ask_screen_shows_the_question_and_the_streaming_answer() {
        let mut app = app_with_findings();
        app.handle(Action::OpenAsk);
        for c in "why is it full".chars() {
            app.handle(Action::Insert(c));
        }
        app.handle(Action::Submit);
        app.handle_event(Event::AnswerToken("Logs have filled the disk.".into()));

        let screen = render(&app, 100, 24);
        assert!(screen.contains("why is it full"), "{screen}");
        assert!(screen.contains("Logs have filled the disk"), "{screen}");
    }

    #[test]
    fn a_failed_answer_is_shown_rather_than_leaving_a_blank_pane() {
        let mut app = App::new(true);
        app.handle(Action::OpenAsk);
        app.answer_state = Answer::Failed("could not reach the model server".into());
        let screen = render(&app, 100, 24);
        assert!(screen.contains("could not reach the model"), "{screen}");
    }

    #[test]
    fn the_help_overlay_states_that_nothing_is_ever_run() {
        let mut app = app_with_findings();
        app.handle(Action::ToggleHelp);
        let screen = render(&app, 100, 30);
        assert!(screen.contains("never runs a command"), "{screen}");
        assert!(screen.contains("copy this finding"), "{screen}");
    }

    #[test]
    fn a_notice_replaces_the_key_hints_on_the_status_line() {
        let mut app = app_with_findings();
        app.handle(Action::Down);
        app.handle(Action::Copy); // the second finding has no command
        let screen = render(&app, 100, 24);
        assert!(screen.contains("no command to copy"), "{screen}");
    }

    #[test]
    fn drawing_survives_a_terminal_far_narrower_than_anyone_would_use() {
        // Layout arithmetic is where TUIs panic. These must not.
        for (w, h) in [(20, 6), (40, 10), (1, 1), (200, 60)] {
            let _ = render(&app_with_findings(), w, h);
        }
    }

    #[test]
    fn every_screen_draws_at_an_ordinary_size() {
        let mut app = app_with_findings();
        for action in [Action::OpenAsk, Action::Back, Action::ToggleHelp] {
            app.handle(action);
            let _ = render(&app, 100, 30);
        }
    }

    #[test]
    fn a_finding_renders_its_evidence_and_its_command() {
        let f = Finding::new("storage.full", Severity::Critical, "/ is 99% full")
            .evidence("1G free of 100G")
            .suggest(Suggestion::cmd("see where it went", "du -sh /"));

        let text: String = detail_lines(&f)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("/ is 99% full"));
        assert!(text.contains("1G free of 100G"));
        assert!(text.contains("du -sh /"));
        assert!(text.contains("id: storage.full"));
        assert!(
            text.contains("does not run these"),
            "every finding must restate that Oracle only suggests"
        );
    }

    #[test]
    fn a_root_command_is_shown_with_sudo_when_we_are_not_root() {
        let f = Finding::new("x", Severity::Warning, "t")
            .suggest(Suggestion::root_cmd("fix", "pacman-db-upgrade"));
        let text: String = detail_lines(&f)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if crate::sys::is_root() {
            assert!(text.contains("pacman-db-upgrade"));
        } else {
            assert!(text.contains("sudo pacman-db-upgrade"), "got {text}");
        }
    }

    #[test]
    fn a_finding_with_nothing_but_a_title_still_renders() {
        let lines = detail_lines(&Finding::new("x", Severity::Note, "bare"));
        assert!(!lines.is_empty());
    }
}
