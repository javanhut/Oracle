//! Overview: the verdict, the counts, the machine, and how Oracle behaves.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use oracle::diagnose;
use oracle::probe::Severity;

use crate::ui::state::{self, Filter};
use crate::ui::{App, widgets};

const HERO_CLASSES: [&str; 4] = ["hero-clean", "hero-note", "hero-warning", "hero-critical"];

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let (root, content) = widgets::page(
        "Overview",
        "What Oracle found when it last looked at this machine. It looks when this window \
         opens and when you ask, never on a timer.",
    );

    // ---- the verdict ----------------------------------------------------
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 22);
    hero.add_css_class("hero");
    let hero_icon = gtk::Image::from_icon_name("content-loading-symbolic");
    hero_icon.add_css_class("hero-icon");
    hero_icon.set_pixel_size(56);
    hero_icon.set_valign(gtk::Align::Center);
    hero.append(&hero_icon);
    let hero_text = gtk::Box::new(gtk::Orientation::Vertical, 6);
    hero_text.set_hexpand(true);
    hero_text.set_valign(gtk::Align::Center);
    let verdict = gtk::Label::new(Some("Checking this machine…"));
    verdict.add_css_class("hero-title");
    verdict.set_xalign(0.0);
    verdict.set_wrap(true);
    hero_text.append(&verdict);
    let when = gtk::Label::new(None);
    when.add_css_class("hero-text");
    when.set_xalign(0.0);
    when.set_wrap(true);
    hero_text.append(&when);
    hero.append(&hero_text);
    let recheck = gtk::Button::with_label("Check again");
    recheck.add_css_class("pill");
    recheck.set_valign(gtk::Align::Center);
    {
        let app = app.clone();
        recheck.connect_clicked(move |_| app.rescan());
    }
    hero.append(&recheck);
    content.append(&hero);

    // ---- the counts -----------------------------------------------------
    let metrics = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    metrics.add_css_class("columns");
    metrics.set_homogeneous(true);
    let (critical_card, critical) = metric(
        app,
        "Critical",
        "Broken now",
        "m-critical",
        Filter::CriticalOnly,
    );
    let (warning_card, warnings) = metric(
        app,
        "Warnings",
        "Broken or about to be, maybe unnoticed",
        "m-warning",
        Filter::WarningsAndWorse,
    );
    let (note_card, notes) = metric(
        app,
        "Notes",
        "Worth knowing, not worth acting on today",
        "m-note",
        Filter::All,
    );
    metrics.append(&critical_card);
    metrics.append(&warning_card);
    metrics.append(&note_card);
    content.append(&metrics);

    let skipped = widgets::dim_label("");
    skipped.set_visible(false);
    content.append(&skipped);

    // ---- the machine, and the rules ---------------------------------------
    let (columns, left, right) = widgets::two_columns();

    let (machine_card, machine_body) = widgets::card("This machine", "");
    let facts = widgets::list();
    let distro = widgets::fact_row("System");
    let kernel = widgets::fact_row("Kernel");
    let init = widgets::fact_row("Init");
    let uptime = widgets::fact_row("Up for");
    let session = widgets::fact_row("Desktop");
    for row in [&distro, &kernel, &init, &uptime, &session] {
        facts.append(row);
    }
    machine_body.append(&facts);
    left.append(&machine_card);

    let (rules_card, rules_body) = widgets::card("How Oracle behaves", "");
    let rules = widgets::list();
    for line in [
        "Looks only while this window is open",
        "Never runs a command. It copies one, and you decide",
        "Talks only to a model on this machine, or to none",
    ] {
        rules.append(&promise_row(line).0);
    }
    let (redaction_row, redaction_icon) = promise_row("Redaction is on");
    rules.append(&redaction_row);
    rules_body.append(&rules);
    right.append(&rules_card);
    content.append(&columns);

    let refresh = move |app: &Rc<App>| {
        let st = app.state.borrow();
        recheck.set_sensitive(!st.scanning);

        if st.config.privacy.redact {
            redaction_row.set_title("Identifiers are redacted before a model sees anything");
            redaction_icon.set_icon_name(Some("object-select-symbolic"));
            redaction_icon.remove_css_class("warning");
        } else {
            redaction_row.set_title("Redaction is off. Settings can turn it back on");
            redaction_icon.set_icon_name(Some("dialog-warning-symbolic"));
            redaction_icon.add_css_class("warning");
        }

        let off = state::skipped_areas(&st.config);
        skipped.set_visible(!off.is_empty());
        skipped.set_text(&format!(
            "Not checked, because Settings says not to look: {}.",
            off.join(", ")
        ));

        if !st.scanned_once {
            verdict.set_text("Checking this machine…");
            when.set_text("Reading services, storage, network, packages, logs and hardware.");
            hero_icon.set_icon_name(Some("content-loading-symbolic"));
            for label in [&critical, &warnings, &notes] {
                label.set_text("–");
            }
            return;
        }

        let worst = st.worst();
        verdict.set_text(&diagnose::summarise(&st.findings));
        when.set_text(&if st.scanning {
            "Checking again…".to_string()
        } else if st.findings.is_empty() {
            format!("Checked at {}. Every check came back clean.", st.checked_at)
        } else {
            format!(
                "Checked at {}. Findings has what Oracle saw and what it would try.",
                st.checked_at
            )
        });
        hero_icon.set_icon_name(Some(state::severity_icon(worst)));
        widgets::set_one_class(
            &hero,
            &HERO_CLASSES,
            match worst {
                None => "hero-clean",
                Some(Severity::Note) => "hero-note",
                Some(Severity::Warning) => "hero-warning",
                Some(Severity::Critical) => "hero-critical",
            },
        );
        critical.set_text(&st.count(Severity::Critical).to_string());
        warnings.set_text(&st.count(Severity::Warning).to_string());
        notes.set_text(&st.count(Severity::Note).to_string());

        let h = &st.host;
        distro.set_subtitle(&widgets::escape(&h.distro));
        kernel.set_subtitle(&widgets::escape(&h.kernel));
        init.set_subtitle(&widgets::escape(&h.init));
        uptime.set_subtitle(&widgets::escape(&h.uptime));
        session.set_subtitle(&widgets::escape(
            h.desktop.as_deref().unwrap_or("none detected"),
        ));
    };
    refresh(app);
    app.on_change(refresh);
    root.upcast()
}

/// A count of one severity, which opens Findings filtered to it.
fn metric(
    app: &Rc<App>,
    label: &str,
    hint: &str,
    class: &str,
    filter: Filter,
) -> (gtk::Button, gtk::Label) {
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let value = gtk::Label::new(Some("–"));
    value.add_css_class("metric-value");
    value.set_xalign(0.0);
    bx.append(&value);
    let name = gtk::Label::new(Some(label));
    name.add_css_class("card-title");
    name.set_xalign(0.0);
    bx.append(&name);
    bx.append(&widgets::dim_label(hint));
    let button = gtk::Button::builder().child(&bx).build();
    button.add_css_class("raven-card");
    button.add_css_class("metric-card");
    button.add_css_class(class);
    button.set_tooltip_text(Some("Show these findings"));
    let app = app.clone();
    button.connect_clicked(move |_| {
        app.set_filter(filter);
        app.navigate("findings");
    });
    (button, value)
}

fn promise_row(text: &str) -> (adw::ActionRow, gtk::Image) {
    let row = adw::ActionRow::builder().title(text).build();
    row.add_css_class("promise");
    let icon = gtk::Image::from_icon_name("object-select-symbolic");
    row.add_prefix(&icon);
    (row, icon)
}
