//! Settings: the model, what Oracle may read, privacy, and leaving.
//!
//! Nothing is written until Save, and then only `~/.config/raven/oracle.toml`
//! -- the same file `oracle setup` writes, so the two front ends never
//! disagree about how Oracle is configured.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use oracle::config::{self, Config};
use oracle::probe::Area;

use crate::ui::state::{self, BACKENDS};
use crate::ui::{App, alert, confirm, widgets};

/// The controls, so they can be filled from a config and read back into one.
struct Form {
    backend: adw::ComboRow,
    endpoint: adw::EntryRow,
    name: adw::EntryRow,
    areas: Vec<(Area, adw::SwitchRow)>,
    redact: adw::SwitchRow,
    remote: adw::SwitchRow,
}

impl Form {
    fn fill(&self, cfg: &Config) {
        self.backend
            .set_selected(state::backend_index(&cfg.model.backend));
        self.endpoint.set_text(&cfg.model.endpoint);
        self.name.set_text(&cfg.model.name);
        for (area, row) in &self.areas {
            row.set_active(state::area_enabled(cfg, *area));
        }
        self.redact.set_active(cfg.privacy.redact);
        self.remote.set_active(cfg.privacy.allow_remote_endpoint);
    }

    /// `base` with the form's values over it, so anything the form does not
    /// show -- timeouts, dictation -- is kept as it was.
    fn read(&self, base: &Config) -> Config {
        let mut cfg = base.clone();
        let (backend, _) = BACKENDS[(self.backend.selected() as usize).min(BACKENDS.len() - 1)];
        cfg.model.backend = backend.to_string();
        cfg.model.endpoint = state::normalise_endpoint(&self.endpoint.text());
        cfg.model.name = self.name.text().trim().to_string();
        for (area, row) in &self.areas {
            state::set_area(&mut cfg, *area, row.is_active());
        }
        cfg.privacy.redact = self.redact.is_active();
        cfg.privacy.allow_remote_endpoint = self.remote.is_active();
        cfg
    }
}

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let (root, content) = widgets::page(
        "Settings",
        "Every setting has a working default. Nothing is written until you press Save, and \
         then only to ~/.config/raven/oracle.toml.",
    );

    // ---- model ----------------------------------------------------------
    let (model_card, model_body) = widgets::card(
        "Local model",
        "Oracle works without one. With one, it can answer questions and explain errors. It \
         never downloads a model or installs a server.",
    );
    let model_list = widgets::list();
    let labels: Vec<&str> = BACKENDS.iter().map(|(_, label)| *label).collect();
    let backend = adw::ComboRow::builder()
        .title("Server")
        .model(&gtk::StringList::new(&labels))
        .build();
    model_list.append(&backend);
    let endpoint = adw::EntryRow::builder().title("Address").build();
    model_list.append(&endpoint);
    let name = adw::EntryRow::builder()
        .title("Model (empty picks a small instruction-tuned one)")
        .build();
    model_list.append(&name);
    let connection = widgets::fact_row("Saved server");
    let check = gtk::Button::with_label("Check");
    check.set_valign(gtk::Align::Center);
    check.set_tooltip_text(Some(
        "Try the server and address above, whether or not they are saved",
    ));
    connection.add_suffix(&check);
    model_list.append(&connection);
    model_body.append(&model_list);
    // What Check found for the values in the form, which may not be the saved
    // ones -- the row above always describes what Ask will actually use.
    let tested = widgets::dim_label("");
    tested.set_visible(false);
    model_body.append(&tested);
    content.append(&model_card);

    // ---- what to read -----------------------------------------------------
    let (areas_card, areas_body) = widgets::card(
        "What Oracle may read",
        "Switching one off means that part of the system is not read at all, so it cannot \
         reach a model by any route. The report says what was not looked at.",
    );
    let areas_list = widgets::list();
    let mut areas = Vec::new();
    for area in Area::all() {
        let row = adw::SwitchRow::builder()
            .title(state::area_title(area))
            .subtitle(state::area_description(area))
            .build();
        areas_list.append(&row);
        areas.push((area, row));
    }
    areas_body.append(&areas_list);
    content.append(&areas_card);

    // ---- privacy ----------------------------------------------------------
    let (privacy_card, privacy_body) = widgets::card("Privacy", "");
    let privacy_list = widgets::list();
    let redact = adw::SwitchRow::builder()
        .title("Redact identifiers")
        .subtitle(
            "Keys, tokens, email and MAC addresses, your username and public IP addresses are \
             removed before a model sees anything",
        )
        .build();
    privacy_list.append(&redact);
    let remote = adw::SwitchRow::builder()
        .title("Allow a model on another machine")
        .subtitle("Off, Oracle refuses to send anything to an address that is not this machine")
        .build();
    privacy_list.append(&remote);
    privacy_body.append(&privacy_list);
    privacy_body.append(&widgets::dim_label(
        "Dictation is set up from a terminal with `oracle voice setup`, and is off until it is.",
    ));
    content.append(&privacy_card);

    let form = Rc::new(Form {
        backend,
        endpoint,
        name,
        areas,
        redact,
        remote,
    });
    form.fill(&app.state.borrow().config);

    {
        let app = app.clone();
        let form = form.clone();
        let tested = tested.clone();
        check.connect_clicked(move |check| {
            let (candidate, unsaved) = {
                let saved = &app.state.borrow().config;
                let candidate = form.read(saved);
                let unsaved = state::model_settings_differ(saved, &candidate);
                (candidate, unsaved)
            };
            check.set_sensitive(false);
            tested.set_visible(true);
            tested.remove_css_class("error");
            tested.remove_css_class("success");
            tested.set_text(&format!("Trying {}…", candidate.model.endpoint));
            let check = check.clone();
            let tested = tested.clone();
            crate::ui::spawn(
                move || crate::ui::probe_model(&candidate),
                move |result| {
                    check.set_sensitive(true);
                    let mut text = if result.ready() {
                        tested.add_css_class("success");
                        format!("Connected: {}.", result.describe())
                    } else {
                        tested.add_css_class("error");
                        result.describe()
                    };
                    if unsaved {
                        text.push_str(
                            "\n\nThese settings are not saved yet. Press Save so Ask and \
                             Explain use them.",
                        );
                    }
                    tested.set_text(&text);
                },
            );
        });
    }

    // Changing the server moves the address to that server's default, unless
    // somebody has already typed an address of their own.
    {
        let form_weak = Rc::downgrade(&form);
        form.backend.connect_selected_notify(move |row| {
            let Some(form) = form_weak.upgrade() else {
                return;
            };
            let current = form.endpoint.text();
            let is_a_default = BACKENDS
                .iter()
                .any(|(b, _)| state::default_endpoint(b) == current.as_str());
            if current.is_empty() || is_a_default {
                let (chosen, _) = BACKENDS[(row.selected() as usize).min(BACKENDS.len() - 1)];
                form.endpoint.set_text(state::default_endpoint(chosen));
            }
        });
    }

    // ---- save -------------------------------------------------------------
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let revert = gtk::Button::with_label("Revert");
    revert.set_tooltip_text(Some("Put back what is saved"));
    {
        let app = app.clone();
        let form = form.clone();
        revert.connect_clicked(move |_| form.fill(&app.state.borrow().config));
    }
    actions.append(&revert);
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    {
        let app = app.clone();
        let form = form.clone();
        save.connect_clicked(move |_| {
            let cfg = form.read(&app.state.borrow().config);
            match app.save_config(cfg) {
                Ok(path) => {
                    // The test result may say "not saved"; it no longer applies.
                    tested.set_visible(false);
                    form.fill(&app.state.borrow().config);
                    app.toast(&format!("Saved to {}", config::tilde(&path)));
                }
                Err(e) => alert(&app.window(), "Not saved", &e),
            }
        });
    }
    actions.append(&save);
    content.append(&actions);

    // ---- files and leaving -------------------------------------------------
    let (files_card, files_body) = widgets::card(
        "Files",
        "Nothing else on the system reads these. Oracle keeps no history of your problems.",
    );
    let files = widgets::list();
    let config_row = widgets::fact_row("Settings");
    files.append(&config_row);
    let state_row = widgets::fact_row("Downloaded dictation models");
    state_row.set_subtitle(&widgets::escape(&config::tilde(&config::state_dir())));
    files.append(&state_row);
    files_body.append(&files);
    let forget_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    forget_row.append(&widgets::dim_label(
        "Forgetting deletes Oracle's settings and anything it downloaded, and goes back to the \
         defaults. The app itself stays installed.",
    ));
    let forget = gtk::Button::with_label("Forget Everything");
    forget.add_css_class("destructive-action");
    forget.set_valign(gtk::Align::Center);
    {
        let app = app.clone();
        let form = form.clone();
        forget.connect_clicked(move |_| {
            let targets = App::written_files();
            if targets.is_empty() {
                app.toast("Oracle has written nothing on this account");
                return;
            }
            let listed: Vec<String> = targets.iter().map(|t| config::tilde(t)).collect();
            let app2 = app.clone();
            let form = form.clone();
            confirm(
                &app.window(),
                "Forget everything?",
                &format!("This deletes:\n\n{}", listed.join("\n")),
                "Delete",
                true,
                move |yes| {
                    if !yes {
                        return;
                    }
                    match app2.forget(&targets) {
                        Ok(()) => app2.toast("Done. Oracle has nothing on this account"),
                        Err(e) => alert(&app2.window(), "Some files were not deleted", &e),
                    }
                    form.fill(&app2.state.borrow().config);
                },
            );
        });
    }
    forget_row.append(&forget);
    files_body.append(&forget_row);
    content.append(&files_card);

    let refresh = move |app: &Rc<App>| {
        let st = app.state.borrow();
        connection.set_subtitle(&widgets::escape(&st.model.describe()));
        config_row.set_subtitle(&widgets::escape(&if Config::exists() {
            config::tilde(&Config::path())
        } else {
            "Not written. Oracle is using its defaults".to_string()
        }));
    };
    refresh(app);
    app.on_change(refresh);
    root.upcast()
}
