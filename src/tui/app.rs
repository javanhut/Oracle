//! The terminal interface's state, with no terminal in sight.
//!
//! Everything here is a plain state machine: keys come in as `Action`s, state
//! changes, and anything with a side effect leaves as a `Task` for the event
//! loop to run. Nothing in this file draws, reads a key, spawns a thread, or
//! touches the system.
//!
//! That split is what makes the interface testable. A TUI is otherwise close
//! to untestable without a terminal to drive, and "the selection went out of
//! range after the filter changed" is exactly the sort of bug that only shows
//! up in front of a user.

use crate::probe::{Finding, Severity};

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The findings list and the detail pane.
    Report,
    /// A question being typed, and the answer streaming back.
    Ask,
    /// Key bindings.
    Help,
}

/// Which pane the keyboard is driving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Detail,
}

/// Which findings are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    WarningsAndWorse,
    CriticalOnly,
}

impl Filter {
    pub fn next(self) -> Filter {
        match self {
            Filter::All => Filter::WarningsAndWorse,
            Filter::WarningsAndWorse => Filter::CriticalOnly,
            Filter::CriticalOnly => Filter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::WarningsAndWorse => "warnings and worse",
            Filter::CriticalOnly => "critical only",
        }
    }

    fn admits(self, s: Severity) -> bool {
        match self {
            Filter::All => true,
            Filter::WarningsAndWorse => s <= Severity::Warning,
            Filter::CriticalOnly => s == Severity::Critical,
        }
    }
}

/// How the model answer is going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Nothing asked yet.
    Idle,
    /// A worker is gathering context and waiting on the model.
    Working,
    /// Tokens are arriving.
    Streaming,
    Done,
    Failed(String),
}

/// A transient line at the bottom of the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub is_error: bool,
}

/// Work the event loop must do outside the state machine.
///
/// The state machine never performs these. It says what it wants, and the loop
/// decides whether and how -- which is the same restraint the rest of Oracle
/// applies to the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    /// Re-run the probes and the rules.
    Rescan,
    /// Send a question to the model.
    Ask(String),
    /// Ask the model to expand on one finding.
    Explain(Finding),
    /// Put text on the clipboard. This is the only thing the interface ever
    /// writes anywhere, and it is not a change to the system.
    Copy(String),
}

/// What a key press means, decided before it reaches the state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Refresh,
    OpenAsk,
    Explain,
    Copy,
    ToggleHelp,
    CycleFilter,
    SwitchFocus,
    /// Leave the current screen, or clear what is on it.
    Back,
    Submit,
    Insert(char),
    Backspace,
    CursorLeft,
    CursorRight,
    Nothing,
}

/// Messages arriving from a worker thread.
#[derive(Debug, Clone)]
pub enum Event {
    ScanStarted,
    ScanFinished(Vec<Finding>),
    AnswerToken(String),
    AnswerFinished,
    AnswerFailed(String),
}

pub struct App {
    pub view: View,
    pub focus: Focus,
    pub filter: Filter,

    pub findings: Vec<Finding>,
    /// Index into the *filtered* list.
    pub selected: usize,
    pub detail_scroll: u16,
    pub scanning: bool,
    /// False until the first scan completes, so the report pane can say it is
    /// working rather than claiming the machine is clean.
    pub scanned_once: bool,

    pub input: String,
    pub cursor: usize,
    pub question: String,
    pub answer: String,
    pub answer_state: Answer,
    pub answer_scroll: u16,

    pub notice: Option<Notice>,
    pub should_quit: bool,
    /// Set when the model is not usable, so the interface can say so once
    /// instead of failing the same way every time somebody presses `a`.
    pub model_available: bool,
}

impl App {
    pub fn new(model_available: bool) -> App {
        App {
            view: View::Report,
            focus: Focus::List,
            filter: Filter::All,
            findings: Vec::new(),
            selected: 0,
            detail_scroll: 0,
            scanning: false,
            scanned_once: false,
            input: String::new(),
            cursor: 0,
            question: String::new(),
            answer: String::new(),
            answer_state: Answer::Idle,
            answer_scroll: 0,
            notice: None,
            should_quit: false,
            model_available,
        }
    }

    /// The findings the current filter admits.
    pub fn visible(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| self.filter.admits(f.severity))
            .collect()
    }

    pub fn selected_finding(&self) -> Option<&Finding> {
        self.visible().get(self.selected).copied()
    }

    /// The first command among the selected finding's suggestions.
    pub fn selected_command(&self) -> Option<String> {
        self.selected_finding()?
            .suggestions
            .iter()
            .find_map(|s| s.command.clone())
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let c = |s: Severity| self.findings.iter().filter(|f| f.severity == s).count();
        (
            c(Severity::Critical),
            c(Severity::Warning),
            c(Severity::Note),
        )
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error: false,
        });
    }

    fn complain(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error: true,
        });
    }

    /// Keep the selection inside the filtered list.
    ///
    /// Called after anything that can shorten the list. Without it, filtering
    /// down to one critical finding while sitting on the tenth note leaves the
    /// detail pane empty and the arrow keys apparently dead.
    fn clamp_selection(&mut self) {
        let len = self.visible().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    /// Apply a key. Returns work for the event loop, if any.
    pub fn handle(&mut self, action: Action) -> Option<Task> {
        // Any key clears a stale notice, so messages do not pile up.
        if !matches!(action, Action::Nothing) {
            self.notice = None;
        }

        match self.view {
            View::Help => self.handle_help(action),
            View::Ask => self.handle_ask(action),
            View::Report => self.handle_report(action),
        }
    }

    fn handle_help(&mut self, action: Action) -> Option<Task> {
        match action {
            // Help is an overlay: anything that would close it closes it, and
            // nothing else does anything, so it cannot be typed into by
            // accident.
            Action::Quit | Action::Back | Action::ToggleHelp => self.view = View::Report,
            _ => {}
        }
        None
    }

    fn handle_ask(&mut self, action: Action) -> Option<Task> {
        match action {
            Action::Back => {
                // First Escape leaves the answer, second leaves the screen.
                if matches!(self.answer_state, Answer::Streaming | Answer::Working) {
                    self.answer_state = Answer::Done;
                    self.notify("stopped");
                } else {
                    self.view = View::Report;
                }
            }
            Action::Quit => self.should_quit = true,
            Action::ToggleHelp => self.view = View::Help,

            Action::Submit => {
                let q = self.input.trim().to_string();
                if q.is_empty() {
                    return None;
                }
                if !self.model_available {
                    self.complain(
                        "no local model is reachable. `oracle status` says what it looked for.",
                    );
                    return None;
                }
                self.question = q.clone();
                self.input.clear();
                self.cursor = 0;
                self.answer.clear();
                self.answer_scroll = 0;
                self.answer_state = Answer::Working;
                return Some(Task::Ask(q));
            }

            Action::Insert(c) => {
                let at = self.byte_index(self.cursor);
                self.input.insert(at, c);
                self.cursor += 1;
            }
            Action::Backspace => {
                if self.cursor > 0 {
                    let at = self.byte_index(self.cursor - 1);
                    self.input.remove(at);
                    self.cursor -= 1;
                }
            }
            Action::CursorLeft => self.cursor = self.cursor.saturating_sub(1),
            Action::CursorRight => {
                self.cursor = (self.cursor + 1).min(self.input.chars().count());
            }
            Action::Home => self.cursor = 0,
            Action::End => self.cursor = self.input.chars().count(),

            Action::Up => self.answer_scroll = self.answer_scroll.saturating_sub(1),
            Action::Down => self.answer_scroll = self.answer_scroll.saturating_add(1),
            Action::PageUp => self.answer_scroll = self.answer_scroll.saturating_sub(10),
            Action::PageDown => self.answer_scroll = self.answer_scroll.saturating_add(10),

            _ => {}
        }
        None
    }

    fn handle_report(&mut self, action: Action) -> Option<Task> {
        match action {
            Action::Quit | Action::Back => self.should_quit = true,
            Action::ToggleHelp => self.view = View::Help,
            Action::OpenAsk => {
                self.view = View::Ask;
                self.focus = Focus::List;
            }

            Action::Up => match self.focus {
                Focus::List => {
                    self.selected = self.selected.saturating_sub(1);
                    self.detail_scroll = 0;
                }
                Focus::Detail => self.detail_scroll = self.detail_scroll.saturating_sub(1),
            },
            Action::Down => match self.focus {
                Focus::List => {
                    let len = self.visible().len();
                    if len > 0 && self.selected + 1 < len {
                        self.selected += 1;
                        self.detail_scroll = 0;
                    }
                }
                Focus::Detail => self.detail_scroll = self.detail_scroll.saturating_add(1),
            },
            Action::PageUp => match self.focus {
                Focus::List => {
                    self.selected = self.selected.saturating_sub(5);
                    self.detail_scroll = 0;
                }
                Focus::Detail => self.detail_scroll = self.detail_scroll.saturating_sub(10),
            },
            Action::PageDown => match self.focus {
                Focus::List => {
                    let len = self.visible().len();
                    if len > 0 {
                        self.selected = (self.selected + 5).min(len - 1);
                        self.detail_scroll = 0;
                    }
                }
                Focus::Detail => self.detail_scroll = self.detail_scroll.saturating_add(10),
            },
            Action::Home => {
                self.selected = 0;
                self.detail_scroll = 0;
            }
            Action::End => {
                self.selected = self.visible().len().saturating_sub(1);
                self.detail_scroll = 0;
            }

            Action::SwitchFocus => {
                self.focus = match self.focus {
                    Focus::List => Focus::Detail,
                    Focus::Detail => Focus::List,
                };
            }

            Action::CycleFilter => {
                self.filter = self.filter.next();
                self.clamp_selection();
                self.detail_scroll = 0;
                let n = self.visible().len();
                self.notify(format!("showing {} ({n})", self.filter.label()));
            }

            Action::Refresh => {
                if self.scanning {
                    return None;
                }
                return Some(Task::Rescan);
            }

            Action::Copy => match self.selected_command() {
                Some(cmd) => return Some(Task::Copy(cmd)),
                None => self.complain("that finding has no command to copy"),
            },

            Action::Explain => {
                if !self.model_available {
                    self.complain(
                        "no local model is reachable, so there is nothing to explain with",
                    );
                    return None;
                }
                match self.selected_finding().cloned() {
                    Some(f) => {
                        self.view = View::Ask;
                        self.question = f.title.clone();
                        self.answer.clear();
                        self.answer_scroll = 0;
                        self.answer_state = Answer::Working;
                        return Some(Task::Explain(f));
                    }
                    None => self.complain("nothing is selected"),
                }
            }

            _ => {}
        }
        None
    }

    /// Apply a message from a worker thread.
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::ScanStarted => self.scanning = true,
            Event::ScanFinished(findings) => {
                self.findings = findings;
                self.scanning = false;
                self.scanned_once = true;
                self.clamp_selection();
            }
            Event::AnswerToken(t) => {
                // A token arriving after the user stopped the stream is
                // discarded rather than reopening a screen they closed.
                if matches!(self.answer_state, Answer::Working | Answer::Streaming) {
                    self.answer_state = Answer::Streaming;
                    self.answer.push_str(&t);
                }
            }
            Event::AnswerFinished => {
                if matches!(self.answer_state, Answer::Working | Answer::Streaming) {
                    self.answer_state = Answer::Done;
                }
            }
            Event::AnswerFailed(e) => {
                if matches!(self.answer_state, Answer::Working | Answer::Streaming) {
                    self.answer_state = Answer::Failed(e);
                }
            }
        }
    }

    /// Byte offset of the `n`th character, for editing the input.
    fn byte_index(&self, n: usize) -> usize {
        self.input
            .char_indices()
            .nth(n)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Suggestion;

    fn finding(id: &str, sev: Severity) -> Finding {
        Finding::new(id, sev, format!("{id} title")).evidence("evidence")
    }

    fn app_with(findings: Vec<Finding>) -> App {
        let mut a = App::new(true);
        a.handle_event(Event::ScanFinished(findings));
        a
    }

    fn sample() -> App {
        app_with(vec![
            finding("a", Severity::Critical),
            finding("b", Severity::Warning),
            finding("c", Severity::Note),
            finding("d", Severity::Note),
        ])
    }

    #[test]
    fn a_fresh_app_has_not_scanned_and_claims_nothing() {
        let a = App::new(false);
        assert!(!a.scanned_once, "it must not look like a clean machine yet");
        assert!(a.findings.is_empty());
        assert_eq!(a.view, View::Report);
    }

    #[test]
    fn the_filter_cycles_and_narrows_what_is_visible() {
        let mut a = sample();
        assert_eq!(a.visible().len(), 4);

        a.handle(Action::CycleFilter);
        assert_eq!(a.filter, Filter::WarningsAndWorse);
        assert_eq!(a.visible().len(), 2);

        a.handle(Action::CycleFilter);
        assert_eq!(a.filter, Filter::CriticalOnly);
        assert_eq!(a.visible().len(), 1);

        a.handle(Action::CycleFilter);
        assert_eq!(a.filter, Filter::All);
        assert_eq!(a.visible().len(), 4);
    }

    #[test]
    fn filtering_pulls_an_out_of_range_selection_back_into_the_list() {
        let mut a = sample();
        a.handle(Action::End);
        assert_eq!(a.selected, 3);

        a.handle(Action::CycleFilter); // warnings and worse: two left
        assert_eq!(a.selected, 1, "the selection must stay on a real row");
        assert!(a.selected_finding().is_some());

        a.handle(Action::CycleFilter); // critical only: one left
        assert_eq!(a.selected, 0);
        assert_eq!(a.selected_finding().unwrap().id, "a");
    }

    #[test]
    fn a_rescan_that_returns_fewer_findings_does_not_strand_the_selection() {
        let mut a = sample();
        a.handle(Action::End);
        a.handle_event(Event::ScanFinished(vec![finding("a", Severity::Critical)]));
        assert_eq!(a.selected, 0);
        assert!(a.selected_finding().is_some());
    }

    #[test]
    fn movement_stops_at_both_ends_rather_than_wrapping_or_running_off() {
        let mut a = sample();
        for _ in 0..10 {
            a.handle(Action::Up);
        }
        assert_eq!(a.selected, 0);
        for _ in 0..10 {
            a.handle(Action::Down);
        }
        assert_eq!(a.selected, 3);
    }

    #[test]
    fn moving_the_selection_resets_the_detail_scroll() {
        let mut a = sample();
        a.handle(Action::SwitchFocus);
        a.handle(Action::Down);
        assert!(a.detail_scroll > 0);
        a.handle(Action::SwitchFocus);
        a.handle(Action::Down);
        assert_eq!(a.detail_scroll, 0, "a new finding starts at its top");
    }

    #[test]
    fn tab_moves_focus_between_the_two_panes() {
        let mut a = sample();
        assert_eq!(a.focus, Focus::List);
        a.handle(Action::SwitchFocus);
        assert_eq!(a.focus, Focus::Detail);
        a.handle(Action::SwitchFocus);
        assert_eq!(a.focus, Focus::List);
    }

    #[test]
    fn copying_asks_for_the_command_and_never_for_it_to_be_run() {
        let mut a = app_with(vec![
            Finding::new("x", Severity::Warning, "t")
                .suggest(Suggestion::new("look first"))
                .suggest(Suggestion::cmd("then this", "du -sh /")),
        ]);
        let task = a.handle(Action::Copy);
        assert_eq!(task, Some(Task::Copy("du -sh /".into())));
    }

    #[test]
    fn copying_a_finding_with_no_command_says_so_instead_of_copying_nothing() {
        let mut a = app_with(vec![finding("x", Severity::Note)]);
        assert_eq!(a.handle(Action::Copy), None);
        assert!(a.notice.as_ref().unwrap().is_error);
    }

    #[test]
    fn refresh_does_not_stack_up_while_a_scan_is_running() {
        let mut a = sample();
        assert_eq!(a.handle(Action::Refresh), Some(Task::Rescan));
        a.handle_event(Event::ScanStarted);
        assert_eq!(a.handle(Action::Refresh), None, "one scan at a time");
        a.handle_event(Event::ScanFinished(vec![]));
        assert_eq!(a.handle(Action::Refresh), Some(Task::Rescan));
    }

    #[test]
    fn asking_without_a_model_explains_rather_than_failing_silently() {
        let mut a = App::new(false);
        a.handle(Action::OpenAsk);
        for c in "why".chars() {
            a.handle(Action::Insert(c));
        }
        assert_eq!(a.handle(Action::Submit), None);
        let notice = a.notice.as_ref().unwrap();
        assert!(notice.is_error);
        assert!(notice.text.contains("no local model"));
    }

    #[test]
    fn explaining_without_a_model_does_not_open_an_empty_screen() {
        let mut a = sample();
        a.model_available = false;
        assert_eq!(a.handle(Action::Explain), None);
        assert_eq!(a.view, View::Report, "it must not navigate on a dead end");
    }

    #[test]
    fn submitting_a_question_clears_the_input_and_starts_working() {
        let mut a = sample();
        a.handle(Action::OpenAsk);
        for c in "why is it slow".chars() {
            a.handle(Action::Insert(c));
        }
        let task = a.handle(Action::Submit);
        assert_eq!(task, Some(Task::Ask("why is it slow".into())));
        assert!(a.input.is_empty());
        assert_eq!(a.cursor, 0);
        assert_eq!(a.answer_state, Answer::Working);
        assert_eq!(a.question, "why is it slow");
    }

    #[test]
    fn an_empty_question_is_not_sent() {
        let mut a = sample();
        a.handle(Action::OpenAsk);
        a.handle(Action::Insert(' '));
        assert_eq!(a.handle(Action::Submit), None);
        assert_eq!(a.answer_state, Answer::Idle);
    }

    #[test]
    fn tokens_accumulate_in_order() {
        let mut a = sample();
        a.answer_state = Answer::Working;
        a.handle_event(Event::AnswerToken("The disk ".into()));
        a.handle_event(Event::AnswerToken("is full.".into()));
        a.handle_event(Event::AnswerFinished);
        assert_eq!(a.answer, "The disk is full.");
        assert_eq!(a.answer_state, Answer::Done);
    }

    #[test]
    fn escape_stops_a_stream_and_later_tokens_are_discarded() {
        let mut a = sample();
        a.handle(Action::OpenAsk);
        a.answer_state = Answer::Streaming;
        a.answer.push_str("partial");

        a.handle(Action::Back);
        assert_eq!(a.answer_state, Answer::Done);
        assert_eq!(
            a.view,
            View::Ask,
            "the first escape stops, it does not leave"
        );

        a.handle_event(Event::AnswerToken(" more".into()));
        assert_eq!(a.answer, "partial", "a stopped stream stays stopped");
    }

    #[test]
    fn a_second_escape_leaves_the_ask_screen() {
        let mut a = sample();
        a.handle(Action::OpenAsk);
        a.handle(Action::Back);
        a.handle(Action::Back);
        assert_eq!(a.view, View::Report);
    }

    #[test]
    fn editing_the_input_respects_the_cursor_and_multibyte_characters() {
        let mut a = App::new(true);
        a.handle(Action::OpenAsk);
        for c in "wifé".chars() {
            a.handle(Action::Insert(c));
        }
        assert_eq!(a.input, "wifé");
        assert_eq!(a.cursor, 4);

        a.handle(Action::CursorLeft);
        a.handle(Action::Insert('X'));
        assert_eq!(a.input, "wifXé");

        a.handle(Action::End);
        a.handle(Action::Backspace);
        assert_eq!(a.input, "wifX", "backspace must not split a character");
    }

    #[test]
    fn backspace_on_an_empty_input_is_harmless() {
        let mut a = App::new(true);
        a.handle(Action::OpenAsk);
        a.handle(Action::Backspace);
        assert!(a.input.is_empty());
        assert_eq!(a.cursor, 0);
    }

    #[test]
    fn help_is_an_overlay_that_swallows_everything_until_it_is_closed() {
        let mut a = sample();
        a.handle(Action::ToggleHelp);
        assert_eq!(a.view, View::Help);

        a.handle(Action::Down);
        a.handle(Action::Insert('q'));
        assert_eq!(a.selected, 0);
        assert!(a.input.is_empty());
        assert!(!a.should_quit, "typing under the overlay must not act");

        a.handle(Action::ToggleHelp);
        assert_eq!(a.view, View::Report);
    }

    #[test]
    fn quit_from_the_report_leaves_and_from_ask_only_after_backing_out() {
        let mut a = sample();
        a.handle(Action::Quit);
        assert!(a.should_quit);

        let mut b = sample();
        b.handle(Action::OpenAsk);
        b.handle(Action::Back);
        assert!(!b.should_quit);
    }

    #[test]
    fn counts_are_reported_by_severity() {
        assert_eq!(sample().counts(), (1, 1, 2));
    }

    #[test]
    fn a_key_press_clears_a_stale_notice() {
        let mut a = app_with(vec![finding("x", Severity::Note)]);
        a.handle(Action::Copy);
        assert!(a.notice.is_some());
        a.handle(Action::Down);
        assert!(a.notice.is_none());
    }
}
