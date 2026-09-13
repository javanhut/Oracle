//! Ask: a question about this machine, answered by the local model.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita::prelude::*;

use crate::ui::answer::Conversation;
use crate::ui::state::ModelState;
use crate::ui::{App, widgets};

/// Starting points, for someone who knows something is wrong and not yet how
/// to put it.
const EXAMPLES: [&str; 4] = [
    "Why can't I install anything?",
    "Why does my Wi-Fi keep dropping?",
    "Why is the machine so slow?",
    "What is filling up my disk?",
];

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let (root, content) = widgets::page(
        "Ask",
        "Ask about this machine in plain language. Oracle reads only the parts of the system \
         the question is about, and hands them to a model running here.",
    );

    let model_note = gtk::Label::new(None);
    model_note.add_css_class("info-note");
    model_note.set_xalign(0.0);
    model_note.set_wrap(true);
    model_note.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    content.append(&model_note);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let entry = gtk::Entry::builder()
        .placeholder_text("What is going wrong?")
        .hexpand(true)
        .build();
    row.append(&entry);
    let send = gtk::Button::with_label("Ask");
    send.add_css_class("suggested-action");
    row.append(&send);
    content.append(&row);

    let examples = gtk::FlowBox::new();
    examples.set_selection_mode(gtk::SelectionMode::None);
    examples.set_max_children_per_line(4);
    examples.set_column_spacing(8);
    examples.set_row_spacing(8);
    for question in EXAMPLES {
        let b = gtk::Button::with_label(question);
        b.add_css_class("pill");
        let app = app.clone();
        b.connect_clicked(move |_| app.ask(question));
        examples.insert(&b, -1);
    }
    content.append(&examples);

    let conversation = Conversation::new(app);
    content.append(conversation.widget());
    *app.ask_conversation.borrow_mut() = Some(conversation);

    let submit = {
        let app = app.clone();
        let entry = entry.clone();
        move || {
            let question = entry.text().trim().to_string();
            if question.is_empty() {
                entry.grab_focus();
                return;
            }
            entry.set_text("");
            app.ask(&question);
        }
    };
    {
        let submit = submit.clone();
        entry.connect_activate(move |_| submit());
    }
    send.connect_clicked(move |_| submit());

    let refresh = move |app: &Rc<App>| {
        let st = app.state.borrow();
        let text = match &st.model {
            ModelState::Checking => st.model.describe(),
            m if m.ready() => format!("Answers come from {}.", m.describe()),
            m => format!(
                "No local model is ready: {} Oracle's checks on Overview and Findings work \
                 without one.",
                m.describe().trim_end_matches('.').to_string() + "."
            ),
        };
        model_note.set_text(&text);
    };
    refresh(app);
    app.on_change(refresh);
    root.upcast()
}
