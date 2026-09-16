//! Explain an error: paste what a command printed, get told what it means.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita::prelude::*;

use oracle::probe::{self, ProbeOptions, SystemView};
use oracle::prompt;

use crate::ui::answer::Conversation;
use crate::ui::{App, widgets};

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let (root, content) = widgets::page(
        "Explain an Error",
        "Paste what a command printed when it failed. Oracle reads the parts of the system the \
         error mentions and asks the local model what it means and what to try first.",
    );

    let (card, body) = widgets::card("The error", "");
    let (well, view) = widgets::text_view(true, true);
    well.set_min_content_height(180);
    body.append(&well);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let paste = gtk::Button::with_label("Paste");
    paste.set_tooltip_text(Some("Paste from the clipboard"));
    buttons.append(&paste);
    let clear = gtk::Button::with_label("Clear");
    buttons.append(&clear);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    buttons.append(&spacer);
    let explain = gtk::Button::with_label("Explain");
    explain.add_css_class("suggested-action");
    buttons.append(&explain);
    body.append(&buttons);
    content.append(&card);

    let conversation = Conversation::new(app, "explain");
    content.append(conversation.widget());

    let buffer = view.buffer();
    {
        let buffer = buffer.clone();
        paste.connect_clicked(move |_| {
            let Some(display) = gtk::gdk::Display::default() else {
                return;
            };
            let clipboard = display.clipboard();
            let buffer = buffer.clone();
            glib::spawn_future_local(async move {
                if let Ok(Some(text)) = clipboard.read_text_future().await {
                    buffer.set_text(&text);
                }
            });
        });
    }
    {
        let buffer = buffer.clone();
        clear.connect_clicked(move |_| buffer.set_text(""));
    }
    {
        let app = app.clone();
        explain.connect_clicked(move |_| {
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            if text.trim().is_empty() {
                app.toast("Paste an error first");
                return;
            }
            let asked = prompt::pasted_error(&text, &app.state.borrow().config);
            conversation.start(&app, "What that error means", &asked, move |cfg| {
                // Guess the subject from the error itself, the same way a
                // question is narrowed.
                let areas = probe::areas_for_question(&text);
                let view = SystemView::gather_for(cfg, &areas, ProbeOptions::default());
                prompt::build_explain(&text, &view, cfg)
            });
        });
    }

    root.upcast()
}
