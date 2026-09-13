//! Building blocks shared by the pages: page headers, cards, rows.

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

/// A page: title, subtitle, and a vertical content box inside a scroller.
pub fn page(title: &str, subtitle: &str) -> (gtk::ScrolledWindow, gtk::Box) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_start(30);
    content.set_margin_end(30);
    content.set_margin_top(22);
    content.set_margin_bottom(30);

    let head = gtk::Box::new(gtk::Orientation::Vertical, 4);
    head.add_css_class("page-head");
    let t = gtk::Label::new(Some(title));
    t.add_css_class("page-title");
    t.set_xalign(0.0);
    head.append(&t);
    if !subtitle.is_empty() {
        let s = gtk::Label::new(Some(subtitle));
        s.add_css_class("page-subtitle");
        s.set_xalign(0.0);
        s.set_wrap(true);
        s.set_margin_bottom(6);
        head.append(&s);
    }
    content.append(&head);

    // Oracle is mostly prose; past a comfortable measure, lines get hard to
    // follow, so the column stops growing.
    let clamp = adw::Clamp::builder()
        .maximum_size(1000)
        .tightening_threshold(760)
        .child(&content)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    (scroller, content)
}

/// A card with a title/subtitle header. Returns (card, body).
pub fn card(title: &str, subtitle: &str) -> (gtk::Box, gtk::Box) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 12);
    outer.add_css_class("raven-card");
    if !title.is_empty() {
        let head = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let t = gtk::Label::new(Some(title));
        t.add_css_class("card-title");
        t.set_xalign(0.0);
        head.append(&t);
        if !subtitle.is_empty() {
            let s = gtk::Label::new(Some(subtitle));
            s.add_css_class("card-subtitle");
            s.set_xalign(0.0);
            s.set_wrap(true);
            head.append(&s);
        }
        outer.append(&head);
    }
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    outer.append(&body);
    (outer, body)
}

/// Two columns of cards. Carries the `columns` class so the window can stack
/// them in a narrow pane.
pub fn two_columns() -> (gtk::Box, gtk::Box, gtk::Box) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.add_css_class("columns");
    row.set_homogeneous(true);
    let left = gtk::Box::new(gtk::Orientation::Vertical, 16);
    let right = gtk::Box::new(gtk::Orientation::Vertical, 16);
    row.append(&left);
    row.append(&right);
    (row, left, right)
}

pub fn dim_label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dim");
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    // Without this a wrapping label still reports its full single-line text
    // as its natural width, which inflates the window past the screen.
    l.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
    l.set_hexpand(true);
    l
}

/// A small heavy caption above a group of lines.
pub fn eyebrow(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("eyebrow");
    l.set_xalign(0.0);
    l
}

pub fn badge(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("badge");
    if !class.is_empty() {
        l.add_css_class(class);
    }
    l.set_valign(gtk::Align::Center);
    l
}

pub fn list() -> gtk::ListBox {
    let l = gtk::ListBox::new();
    l.add_css_class("boxed-list");
    l.set_selection_mode(gtk::SelectionMode::None);
    l
}

pub fn clear_box(bx: &gtk::Box) {
    while let Some(child) = bx.first_child() {
        bx.remove(&child);
    }
}

pub fn empty_state(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(description)
        .vexpand(true)
        .build()
}

/// A row that names a fact; the value goes in the subtitle, selectable.
pub fn fact_row(title: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle("–")
        .subtitle_selectable(true)
        .build()
}

/// Rows take markup; text from the machine must not be read as any.
pub fn escape(text: &str) -> String {
    glib::markup_escape_text(text).to_string()
}

/// A text area in a rounded well, for pasted errors and long context.
pub fn text_view(monospace: bool, editable: bool) -> (gtk::ScrolledWindow, gtk::TextView) {
    let view = gtk::TextView::new();
    view.set_editable(editable);
    view.set_cursor_visible(editable);
    view.set_monospace(monospace);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_left_margin(4);
    view.set_right_margin(4);
    view.set_top_margin(4);
    view.set_bottom_margin(4);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&view)
        .build();
    scroller.add_css_class("text-well");
    (scroller, view)
}

/// Give `widget` exactly one of `classes`.
pub fn set_one_class(widget: &impl IsA<gtk::Widget>, classes: &[&str], on: &str) {
    for c in classes {
        widget.remove_css_class(c);
    }
    widget.add_css_class(on);
}

/// Stack (or unstack) every two-column row under `root`.
pub fn set_columns_stacked(root: &impl IsA<gtk::Widget>, stacked: bool) {
    fn walk(w: &gtk::Widget, stacked: bool) {
        if w.has_css_class("columns")
            && let Some(b) = w.downcast_ref::<gtk::Box>()
        {
            b.set_orientation(if stacked {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            });
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            walk(&ch, stacked);
            c = ch.next_sibling();
        }
    }
    walk(root.upcast_ref(), stacked);
}
