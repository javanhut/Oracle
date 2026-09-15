//! Desktop notifications, for an answer that arrives while nobody is looking.
//!
//! A big model can take minutes, and nobody should have to watch a spinner
//! for that long. When an answer finishes and the window is hidden, not
//! focused, or on another page, Oracle says so with a notification. Clicking
//! it brings the window back on the page with the answer.
//!
//! Notifications go straight to `org.freedesktop.Notifications` on the session
//! bus. GIO's own `send_notification` needs the application registered on the
//! bus, which a `NON_UNIQUE` app is not, and it fails silently. Here, a
//! desktop with nothing to show notifications is an answer the caller hears,
//! so a hidden window can come back on its own instead.
//!
//! The decisions -- whether to notify, and what each response means -- are
//! plain functions, so they are tested without a bus.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gio::prelude::*;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// How long a preview of the answer may be, in characters.
const PREVIEW: usize = 160;

/// What happened to a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Nothing on this desktop shows notifications.
    Unavailable,
    /// The person clicked it.
    Opened,
    Expired,
    Dismissed,
    /// Closed some other way, such as by another program.
    Closed,
}

impl Event {
    /// The `reason` of a `NotificationClosed` signal.
    fn closed(reason: u32) -> Event {
        match reason {
            1 => Event::Expired,
            2 => Event::Dismissed,
            _ => Event::Closed,
        }
    }
}

/// Which notification an event is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The window was closed while an answer was still coming.
    Working,
    /// An answer finished, or failed.
    Ready,
}

/// What the app should do about an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    Nothing,
    /// Bring the window back, on the page with the answer.
    Show,
    /// End the app: the window is hidden and the answer has been dealt with.
    Quit,
}

/// The text of a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub summary: String,
    pub body: String,
}

/// Whether an answer that just ended is worth a notification. Not when the
/// person is already looking at it.
pub fn worth_notifying(visible: bool, focused: bool, on_page: bool) -> bool {
    !(visible && focused && on_page)
}

/// What to do about `event`, given whether the window is hidden and whether
/// any answer is still coming.
pub fn respond(kind: Kind, event: Event, hidden: bool, answering: bool) -> Response {
    match (kind, event) {
        (_, Event::Opened) => Response::Show,
        // The "still answering" notice is a courtesy. Losing it changes
        // nothing: the answer comes back either way.
        (Kind::Working, _) => Response::Nothing,
        // No way to tell a hidden window's owner the answer is ready, so the
        // window comes back itself.
        (Kind::Ready, Event::Unavailable) if hidden => Response::Show,
        // Dismissed without opening: the person has seen it and does not want
        // the window. A hidden window has nothing else to do, unless another
        // answer is still coming.
        (Kind::Ready, Event::Dismissed) if hidden && !answering => Response::Quit,
        // Expired unseen: without the notification, a hidden window would be
        // unreachable, so it comes back rather than lingering.
        (Kind::Ready, Event::Expired) if hidden => Response::Show,
        (Kind::Ready, _) => Response::Nothing,
    }
}

/// What to say when an answer ends. `text` is the answer, or the reason there
/// is none.
pub fn report(heading: &str, failed: bool, text: &str) -> Report {
    let summary = if failed {
        format!("Oracle could not answer: {heading}")
    } else {
        format!("Answer ready: {heading}")
    };
    Report {
        summary,
        body: escape(&preview(text)),
    }
}

/// The notice for a window closed while an answer is still coming.
pub fn working() -> Report {
    Report {
        summary: "Oracle is still answering".into(),
        body: "The window is hidden until the answer is ready. You will get a notification \
               then."
            .into(),
    }
}

/// The start of `text` on one line, cut at a word where possible.
fn preview(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PREVIEW {
        return flat;
    }
    let cut: String = flat.chars().take(PREVIEW).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > PREVIEW / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end_matches([',', '.', ';', ':']))
}

/// Notification bodies may be read as markup. An answer about a shell
/// command is full of `<`, `>` and `&`, which must arrive as text.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The arguments of `Notify`, replacing notification `replaces` (0 for none).
/// A server rejects a call whose types do not match its signature exactly.
fn notify_params(replaces: u32, report: &Report) -> glib::Variant {
    let hints: HashMap<String, glib::Variant> = HashMap::from([
        ("desktop-entry".to_string(), super::APP_ID.to_variant()),
        // Normal urgency: an answer is news, not an alarm.
        ("urgency".to_string(), 1u8.to_variant()),
    ]);
    (
        "Raven Oracle".to_string(),
        replaces,
        super::APP_ID.to_string(),
        report.summary.clone(),
        report.body.clone(),
        // "default" is the action a click on the notification itself invokes.
        vec!["default".to_string(), "Open".to_string()],
        hints,
        // -1: the server's usual timeout.
        -1i32,
    )
        .to_variant()
}

type Handler = Rc<dyn Fn(Event)>;

/// Oracle's one notification. A new one replaces the last rather than
/// stacking, and only the one on screen reports back.
#[derive(Default)]
pub struct Notifier {
    connection: RefCell<Option<gio::DBusConnection>>,
    subscriptions: RefCell<Vec<gio::SignalSubscription>>,
    /// The server's id for the notification on screen; 0 for none.
    shown: Cell<u32>,
    handler: RefCell<Option<Handler>>,
}

impl Notifier {
    /// Show `report`. `on_event` hears what becomes of it, including that it
    /// could not be shown at all.
    pub fn send(self: &Rc<Self>, report: &Report, on_event: impl Fn(Event) + 'static) {
        let this = self.clone();
        let report = report.clone();
        let on_event: Handler = Rc::new(on_event);
        glib::spawn_future_local(async move {
            let Some(connection) = this.connection().await else {
                on_event(Event::Unavailable);
                return;
            };
            let params = notify_params(this.shown.get(), &report);
            let reply = connection
                .call_future(
                    Some(NAME),
                    PATH,
                    NAME,
                    "Notify",
                    Some(&params),
                    glib::VariantTy::new("(u)").ok(),
                    gio::DBusCallFlags::NONE,
                    5000,
                )
                .await;
            match reply.ok().and_then(|v| v.get::<(u32,)>()) {
                Some((id,)) => {
                    this.shown.set(id);
                    *this.handler.borrow_mut() = Some(on_event);
                }
                None => on_event(Event::Unavailable),
            }
        });
    }

    /// The session bus, connected once, with the signals that report back.
    async fn connection(self: &Rc<Self>) -> Option<gio::DBusConnection> {
        if let Some(c) = self.connection.borrow().clone() {
            return Some(c);
        }
        let connection = gio::bus_get_future(gio::BusType::Session).await.ok()?;
        let mut subscriptions = Vec::new();
        for signal in ["ActionInvoked", "NotificationClosed"] {
            let weak = Rc::downgrade(self);
            subscriptions.push(connection.subscribe_to_signal(
                Some(NAME),
                Some(NAME),
                Some(signal),
                Some(PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |s| {
                    let Some(this) = weak.upgrade() else { return };
                    let heard = if s.signal_name == "ActionInvoked" {
                        s.parameters
                            .get::<(u32, String)>()
                            .map(|(id, _)| (id, Event::Opened))
                    } else {
                        s.parameters
                            .get::<(u32, u32)>()
                            .map(|(id, reason)| (id, Event::closed(reason)))
                    };
                    if let Some((id, event)) = heard {
                        this.dispatch(id, event);
                    }
                },
            ));
        }
        *self.subscriptions.borrow_mut() = subscriptions;
        *self.connection.borrow_mut() = Some(connection.clone());
        Some(connection)
    }

    fn dispatch(&self, id: u32, event: Event) {
        if id == 0 || id != self.shown.get() {
            return;
        }
        let handler = if event == Event::Opened {
            self.handler.borrow().clone()
        } else {
            // Closed for good: nothing more will be heard about this one.
            self.shown.set(0);
            self.handler.take()
        };
        if let Some(handler) = handler {
            handler(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_is_called_with_the_signature_the_specification_gives() {
        let params = notify_params(7, &report("q", false, "answer"));
        assert_eq!(params.type_().as_str(), "(susssasa{sv}i)");
        assert_eq!(params.child_value(1).get::<u32>(), Some(7), "replaces_id");
        let hints = glib::VariantDict::new(Some(&params.child_value(6)));
        assert_eq!(
            hints.lookup::<u8>("urgency").ok().flatten(),
            Some(1),
            "urgency must be a byte, or servers ignore it"
        );
    }

    #[test]
    fn nobody_is_told_about_an_answer_they_are_looking_at() {
        assert!(!worth_notifying(true, true, true));
        assert!(worth_notifying(false, false, true), "hidden");
        assert!(
            worth_notifying(true, false, true),
            "another window has focus"
        );
        assert!(worth_notifying(true, true, false), "on another page");
    }

    #[test]
    fn opening_either_notification_brings_the_window_back() {
        for kind in [Kind::Working, Kind::Ready] {
            assert_eq!(respond(kind, Event::Opened, true, false), Response::Show);
        }
    }

    #[test]
    fn a_hidden_window_comes_back_when_nothing_can_show_a_notification() {
        assert_eq!(
            respond(Kind::Ready, Event::Unavailable, true, false),
            Response::Show
        );
        assert_eq!(
            respond(Kind::Ready, Event::Unavailable, false, false),
            Response::Nothing,
            "a window already on screen needs nothing"
        );
        assert_eq!(
            respond(Kind::Working, Event::Unavailable, true, true),
            Response::Nothing,
            "the answer is not ready yet; showing the window now defeats hiding it"
        );
    }

    #[test]
    fn dismissing_the_answer_ends_a_hidden_app_only_when_nothing_else_is_coming() {
        assert_eq!(
            respond(Kind::Ready, Event::Dismissed, true, false),
            Response::Quit
        );
        assert_eq!(
            respond(Kind::Ready, Event::Dismissed, true, true),
            Response::Nothing
        );
        assert_eq!(
            respond(Kind::Ready, Event::Dismissed, false, false),
            Response::Nothing,
            "a visible window is never closed from a notification"
        );
        assert_eq!(
            respond(Kind::Working, Event::Dismissed, true, true),
            Response::Nothing
        );
    }

    #[test]
    fn an_expired_answer_never_leaves_a_hidden_window_unreachable() {
        assert_eq!(
            respond(Kind::Ready, Event::Expired, true, false),
            Response::Show
        );
        assert_eq!(
            respond(Kind::Ready, Event::Expired, false, false),
            Response::Nothing
        );
    }

    #[test]
    fn closed_reasons_are_read_as_the_specification_numbers_them() {
        assert_eq!(Event::closed(1), Event::Expired);
        assert_eq!(Event::closed(2), Event::Dismissed);
        assert_eq!(Event::closed(3), Event::Closed);
        assert_eq!(Event::closed(4), Event::Closed);
    }

    #[test]
    fn a_report_names_the_question_and_previews_the_answer() {
        let r = report("raven-keycast isn't working", false, "Check the\n  unit.");
        assert_eq!(r.summary, "Answer ready: raven-keycast isn't working");
        assert_eq!(r.body, "Check the unit.");

        let r = report(
            "Why is it slow?",
            true,
            "the model server reported an error",
        );
        assert!(r.summary.starts_with("Oracle could not answer"));
        assert!(r.body.contains("reported an error"));
    }

    #[test]
    fn a_long_answer_is_cut_at_a_word() {
        let long = "word ".repeat(100);
        let body = report("q", false, &long).body;
        assert!(body.ends_with('…'), "got {body}");
        assert!(body.chars().count() <= PREVIEW + 1);
        assert!(!body.contains("wor…"), "cut mid-word: {body}");
    }

    #[test]
    fn shell_text_in_an_answer_is_not_read_as_markup() {
        let body = report("q", false, "run `ls <dir> && echo ok`").body;
        assert_eq!(body, "run `ls &lt;dir&gt; &amp;&amp; echo ok`");
    }
}
