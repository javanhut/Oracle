//! One conversation with the local model: a heading, a status line, and the
//! answer as it streams in. Ask and Explain each own one.
//!
//! The model call runs on a thread of its own and sends tokens back over a
//! channel the main loop drains. Starting a new answer, or pressing Stop,
//! bumps a generation counter and raises a cancel flag, so a stopped answer
//! stays stopped and an old one can never overwrite a newer one.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::time::Duration;

use gtk4 as gtk;
use libadwaita::prelude::*;

use oracle::config::Config;
use oracle::model::{self, Message, ModelError};

use super::{App, state, widgets};

enum Update {
    /// The context is gathered and the question is with the model.
    Asking(&'static str),
    Token(String),
    Finished,
    Failed(String),
}

pub struct Conversation {
    root: gtk::Box,
    heading: gtk::Label,
    spinner: gtk::Spinner,
    status: gtk::Label,
    stop: gtk::Button,
    copy: gtk::Button,
    buffer: gtk::TextBuffer,
    generation: Cell<u64>,
    cancel: RefCell<Arc<AtomicBool>>,
}

impl Conversation {
    pub fn new(app: &Rc<App>) -> Rc<Conversation> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("raven-card");
        root.set_visible(false);

        let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let spinner = gtk::Spinner::new();
        spinner.set_valign(gtk::Align::Center);
        top.append(&spinner);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let heading = gtk::Label::new(None);
        heading.add_css_class("card-title");
        heading.set_xalign(0.0);
        heading.set_wrap(true);
        heading.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        text.append(&heading);
        let status = widgets::dim_label("");
        text.append(&status);
        top.append(&text);
        let stop = gtk::Button::with_label("Stop");
        stop.set_valign(gtk::Align::Center);
        stop.set_tooltip_text(Some("Stop generating this answer"));
        top.append(&stop);
        let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy.add_css_class("flat");
        copy.set_valign(gtk::Align::Center);
        copy.set_tooltip_text(Some("Copy the answer"));
        copy.set_visible(false);
        top.append(&copy);
        root.append(&top);

        let buffer = gtk::TextBuffer::new(None);
        let view = gtk::TextView::with_buffer(&buffer);
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.add_css_class("answer");
        root.append(&view);

        let conversation = Rc::new(Conversation {
            root,
            heading,
            spinner,
            status,
            stop,
            copy,
            buffer,
            generation: Cell::new(0),
            cancel: RefCell::new(Arc::new(AtomicBool::new(false))),
        });
        {
            let weak = Rc::downgrade(&conversation);
            conversation.stop.connect_clicked(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.halt();
                }
            });
        }
        {
            let app = app.clone();
            let buffer = conversation.buffer.clone();
            conversation.copy.connect_clicked(move |_| {
                let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
                app.copy(text.as_str(), "the answer");
            });
        }
        conversation
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Start an answer. `build` runs on the worker: it gathers whatever part
    /// of the system the request is about and assembles the conversation.
    pub fn start<F>(self: &Rc<Self>, app: &Rc<App>, heading: &str, build: F)
    where
        F: FnOnce(&Config) -> Vec<Message> + Send + 'static,
    {
        self.cancel.borrow().store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.borrow_mut() = cancel.clone();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);

        self.root.set_visible(true);
        self.heading.set_text(heading);
        self.buffer.set_text("");
        self.status.remove_css_class("error");
        self.status
            .set_text("Reading the parts of the system this is about…");
        self.spinner.set_visible(true);
        self.spinner.start();
        self.stop.set_visible(true);
        self.copy.set_visible(false);

        let cfg = app.state.borrow().config.clone();
        let (tx, rx) = mpsc::channel::<Update>();
        std::thread::spawn(move || converse(cfg, build, cancel, tx));

        let conversation = self.clone();
        glib::timeout_add_local(Duration::from_millis(40), move || {
            if conversation.generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            loop {
                match rx.try_recv() {
                    Ok(Update::Asking(backend)) => conversation
                        .status
                        .set_text(&format!("Waiting for the model on {backend}…")),
                    Ok(Update::Token(t)) => {
                        let mut end = conversation.buffer.end_iter();
                        conversation.buffer.insert(&mut end, &t);
                        conversation.status.set_text("Answering…");
                    }
                    Ok(Update::Finished) => {
                        conversation.finish(
                            "From the local model. The findings are certain; this is not, so \
                             check it against them.",
                            false,
                        );
                        return glib::ControlFlow::Break;
                    }
                    Ok(Update::Failed(e)) => {
                        conversation.finish(&e, true);
                        return glib::ControlFlow::Break;
                    }
                    Err(TryRecvError::Empty) => return glib::ControlFlow::Continue,
                    Err(TryRecvError::Disconnected) => {
                        conversation.finish("The model stopped without finishing.", true);
                        return glib::ControlFlow::Break;
                    }
                }
            }
        });
    }

    fn halt(&self) {
        self.cancel.borrow().store(true, Ordering::Relaxed);
        self.generation.set(self.generation.get() + 1);
        self.finish(
            "Stopped. The model was told to stop, and anything it sends now is dropped.",
            false,
        );
    }

    fn finish(&self, status: &str, failed: bool) {
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.stop.set_visible(false);
        self.copy.set_visible(self.buffer.char_count() > 0);
        self.status.set_text(status);
        if failed {
            self.status.add_css_class("error");
        }
    }
}

/// The worker: gather, ask, forward tokens until done or told to stop.
///
/// The backend is built here rather than passed in: a `Box<dyn Backend>` is
/// not `Send`, and building one costs nothing.
fn converse<F>(cfg: Config, build: F, cancel: Arc<AtomicBool>, tx: Sender<Update>)
where
    F: FnOnce(&Config) -> Vec<Message>,
{
    if let Some(problem) = state::endpoint_problem(&cfg) {
        let _ = tx.send(Update::Failed(problem));
        return;
    }
    let messages = build(&cfg);
    let backend = match model::backend_for(&cfg) {
        Ok(b) => b,
        Err(e) => {
            let _ = tx.send(Update::Failed(format!(
                "{e}\n\n{}",
                state::no_model_advice(&cfg)
            )));
            return;
        }
    };
    let _ = tx.send(Update::Asking(backend.name()));

    let result = backend.chat(&messages, &mut |token| {
        // A stop, or a closed window, ends generation rather than letting the
        // server finish an answer nobody will read.
        !cancel.load(Ordering::Relaxed) && tx.send(Update::Token(token.to_string())).is_ok()
    });

    let _ = tx.send(match result {
        Ok(_) | Err(ModelError::Interrupted) => Update::Finished,
        Err(
            e @ (ModelError::NotConfigured | ModelError::Unreachable(_) | ModelError::NoModel(_)),
        ) => Update::Failed(format!("{e}\n\n{}", state::no_model_advice(&cfg))),
        Err(e) => Update::Failed(e.to_string()),
    });
}
