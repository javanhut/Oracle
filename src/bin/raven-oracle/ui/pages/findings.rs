//! Findings: worst first, each with what Oracle read and what it would try.

use std::cell::Cell;
use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use oracle::probe::{Finding, Severity};

use crate::ui::state::{self, Filter};
use crate::ui::{App, widgets};

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let (root, content) = widgets::page(
        "Findings",
        "Worst first. Each one shows what Oracle read and what it would try. Oracle does not \
         run any of it: copy a command and decide for yourself.",
    );

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let segmented = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    segmented.add_css_class("linked");
    segmented.add_css_class("segmented");
    let mut toggles: Vec<gtk::ToggleButton> = Vec::new();
    for filter in Filter::ALL {
        let b = gtk::ToggleButton::with_label(filter.label());
        if let Some(first) = toggles.first() {
            b.set_group(Some(first));
        }
        b.set_active(filter == app.filter.get());
        let app = app.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                app.set_filter(filter);
            }
        });
        segmented.append(&b);
        toggles.push(b);
    }
    bar.append(&segmented);
    let count = gtk::Label::new(None);
    count.add_css_class("dim");
    count.set_hexpand(true);
    count.set_xalign(1.0);
    bar.append(&count);
    content.append(&bar);

    let holder = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&holder);

    // Rebuilding the list collapses every open finding, so it happens only
    // when there is something new to show.
    let shown: Cell<Option<(u64, Filter, bool)>> = Cell::new(None);
    let refresh = move |app: &Rc<App>| {
        let filter = app.filter.get();
        if let Some(i) = Filter::ALL.iter().position(|f| *f == filter)
            && !toggles[i].is_active()
        {
            toggles[i].set_active(true);
        }

        let st = app.state.borrow();
        let key = (st.revision, filter, st.scanned_once);
        if shown.get() == Some(key) {
            return;
        }
        shown.set(Some(key));
        widgets::clear_box(&holder);

        if !st.scanned_once {
            count.set_text("");
            holder.append(&widgets::empty_state(
                "content-loading-symbolic",
                "Checking this machine",
                "Findings appear here as soon as the checks finish.",
            ));
            return;
        }

        let visible = st.visible(filter);
        count.set_text(&format!("{} of {}", visible.len(), st.findings.len()));
        if st.findings.is_empty() {
            let skipped = state::skipped_areas(&st.config);
            let description = if skipped.is_empty() {
                "Every check Oracle ran came back clean.".to_string()
            } else {
                format!(
                    "Every check Oracle ran came back clean. It did not look at {}, because \
                     Settings says not to.",
                    skipped.join(", ")
                )
            };
            holder.append(&widgets::empty_state(
                "object-select-symbolic",
                "Nothing to report",
                &description,
            ));
        } else if visible.is_empty() {
            holder.append(&widgets::empty_state(
                "object-select-symbolic",
                "Nothing at this level",
                "Everything Oracle found is less serious than this. Choose Everything to see it.",
            ));
        } else {
            let list = widgets::list();
            for finding in visible {
                list.append(&finding_row(app, finding, st.host.root));
            }
            holder.append(&list);
        }
    };
    refresh(app);
    app.on_change(refresh);
    root.upcast()
}

fn finding_row(app: &Rc<App>, finding: &Finding, root: bool) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title(widgets::escape(&finding.title))
        .subtitle(state::severity_label(finding.severity))
        .build();
    let icon = gtk::Image::from_icon_name(state::severity_icon(Some(finding.severity)));
    icon.add_css_class(state::severity_class(Some(finding.severity)));
    row.add_prefix(&icon);
    // Critical findings open by default: they are the ones worth reading now.
    row.set_expanded(finding.severity == Severity::Critical);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.add_css_class("finding-body");

    if !finding.evidence.is_empty() {
        body.append(&widgets::eyebrow("WHAT ORACLE SAW"));
        for e in &finding.evidence {
            let l = widgets::dim_label(e);
            l.add_css_class("evidence");
            l.set_selectable(true);
            body.append(&l);
        }
    }

    if !finding.suggestions.is_empty() {
        body.append(&widgets::eyebrow("WHAT TO TRY"));
        for s in &finding.suggestions {
            let what = gtk::Label::new(Some(&s.what));
            what.set_xalign(0.0);
            what.set_wrap(true);
            what.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            body.append(&what);
            if let Some(cmd) = state::shown_command(s, root) {
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let shown = gtk::Label::new(Some(&cmd));
                shown.add_css_class("command");
                shown.set_xalign(0.0);
                shown.set_hexpand(true);
                shown.set_selectable(true);
                shown.set_wrap(true);
                shown.set_wrap_mode(gtk::pango::WrapMode::Char);
                line.append(&shown);
                let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
                copy.add_css_class("flat");
                copy.set_valign(gtk::Align::Center);
                copy.set_tooltip_text(Some("Copy this command"));
                let app = app.clone();
                copy.connect_clicked(move |_| app.copy(&cmd, "the command"));
                line.append(&copy);
                body.append(&line);
            }
        }
    }

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    footer.set_margin_top(6);
    footer.append(&widgets::dim_label(
        "Oracle does not run these. Copy one and decide.",
    ));
    let explain = gtk::Button::with_label("Explain this");
    explain.set_valign(gtk::Align::Center);
    explain.set_tooltip_text(Some(
        "Have the local model say what this means and what to check first",
    ));
    {
        let app = app.clone();
        let finding = finding.clone();
        explain.connect_clicked(move |_| app.explain_finding(&finding));
    }
    footer.append(&explain);
    body.append(&footer);

    row.add_row(&body);
    row
}
