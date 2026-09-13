//! What a model sees: exactly the text Oracle would send, shown before
//! anything is. The privacy claims are checkable here rather than believable.

use std::cell::Cell;
use std::rc::Rc;

use gtk4 as gtk;
use libadwaita::prelude::*;

use crate::ui::{App, widgets};

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let (root, content) = widgets::page(
        "What a Model Sees",
        "Everything below is what Oracle would hand to a model, as read at the last check. \
         Nothing on this page has been sent. A question about one part of the system sends \
         less than this.",
    );

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let redaction = widgets::badge("", "");
    bar.append(&redaction);
    let when = widgets::dim_label("");
    bar.append(&when);
    let copy = gtk::Button::with_label("Copy");
    copy.set_tooltip_text(Some("Copy all of it"));
    {
        let app = app.clone();
        copy.connect_clicked(move |_| {
            let text = app.state.borrow().context.clone();
            app.copy(&text, "the context");
        });
    }
    bar.append(&copy);
    content.append(&bar);

    let (well, view) = widgets::text_view(true, false);
    well.set_min_content_height(460);
    well.set_vexpand(true);
    content.append(&well);

    let shown = Cell::new(u64::MAX);
    let refresh = move |app: &Rc<App>| {
        let st = app.state.borrow();
        if st.config.privacy.redact {
            redaction.set_text("redaction on");
            widgets::set_one_class(&redaction, &["success", "warning"], "success");
        } else {
            redaction.set_text("redaction OFF");
            widgets::set_one_class(&redaction, &["success", "warning"], "warning");
        }
        copy.set_sensitive(st.scanned_once);
        if !st.scanned_once {
            when.set_text("Reading the machine…");
            view.buffer().set_text("");
            return;
        }
        when.set_text(&if st.scanning {
            "Checking again…".to_string()
        } else {
            format!("As read at {}", st.checked_at)
        });
        if shown.get() != st.revision {
            shown.set(st.revision);
            view.buffer().set_text(&st.context);
        }
    };
    refresh(app);
    app.on_change(refresh);
    root.upcast()
}
