//! The look: Raven Glass, the stylesheet shared with Settings, Store and Power
//! (`data/raven-glass.css`, kept identical across the repos), plus the classes
//! only Oracle draws -- the verdict hero, severity colours, command wells.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::desktop::{Desktop, ThemeMode};

pub const BASE_CSS: &str = concat!(
    include_str!("../../../../data/raven-glass.css"),
    r#"
/* ── Raven Oracle ────────────────────────────────────────────────────── */

/* The rule above Settings is a line, not a disabled row. */
.sidebar list.navigation-sidebar row.separator-row,
.sidebar list.navigation-sidebar row.separator-row:disabled {
  background: none; box-shadow: none; min-height: 0; padding: 0; margin: 6px 0;
}
.sidebar list.navigation-sidebar row.separator-row separator { margin: 0 8px; }

/* The verdict: one big card whose backdrop says how the machine is. */
.hero {
  border-radius: 20px;
  padding: 24px 28px;
  border: 1px solid alpha(#ffffff, 0.12);
  box-shadow: inset 0 1px 0 alpha(#ffffff, 0.18), 0 12px 32px alpha(#000000, 0.30);
  background-image: linear-gradient(135deg, #17254f, #2f2264 55%, #66358a);
}
.hero.hero-clean    { background-image: linear-gradient(135deg, #0f2a30, #194b3e 55%, #27754d); }
.hero.hero-note     { background-image: linear-gradient(135deg, #17254f, #2a3f7a 55%, #3c5fa8); }
.hero.hero-warning  { background-image: linear-gradient(135deg, #33240c, #6a4613 55%, #9a6a1c); }
.hero.hero-critical { background-image: linear-gradient(135deg, #2f1641, #66274b 55%, #9e3d5b); }
.hero .hero-title { font-size: 26px; font-weight: 800; letter-spacing: -0.7px; color: #ffffff; }
.hero .hero-text { font-size: 14px; color: alpha(#ffffff, 0.82); }
.hero .hero-icon { color: #ffffff; opacity: 0.95; }
.hero button.pill {
  border-radius: 999px; padding: 0 18px; min-height: 34px;
  background-color: alpha(#ffffff, 0.16); color: #ffffff;
  border: 1px solid alpha(#ffffff, 0.28);
  box-shadow: inset 0 1px 0 alpha(#ffffff, 0.22);
}
.hero button.pill:hover { background-color: alpha(#ffffff, 0.26); }
.hero button.pill:active { background-color: alpha(#ffffff, 0.34); }

/* Severity counts; each opens Findings at its level. */
button.metric-card { padding: 16px; min-height: 96px; }
button.metric-card:hover { background-color: alpha(#ffffff, 0.09); border-color: alpha(#ffffff, 0.14); }
.metric-value { font-size: 30px; font-weight: 700; letter-spacing: -0.8px; }
.m-critical .metric-value { color: @error_color; }
.m-warning .metric-value { color: @warning_color; }
.m-note .metric-value { color: @accent_bg_color; }

image.sev-clean, label.sev-clean { color: @success_color; }
image.sev-critical, label.sev-critical { color: @error_color; }
image.sev-warning, label.sev-warning { color: @warning_color; }
image.sev-note, label.sev-note { color: @accent_bg_color; }

/* A finding, opened. */
.finding-body { padding: 4px 14px 16px 14px; }
.finding-body .eyebrow { margin-top: 6px; }
.evidence { font-size: 13px; }

/* A command: shown, never run. */
.command {
  font-family: monospace; font-size: 12.5px;
  background-color: alpha(#000000, 0.28);
  border: 1px solid alpha(#ffffff, 0.08);
  border-radius: 8px;
  padding: 6px 10px;
}

.text-well {
  background-color: alpha(#000000, 0.25);
  border: 1px solid alpha(#ffffff, 0.08);
  border-radius: 12px;
  padding: 8px;
}
.text-well text, .text-well textview { background-color: transparent; }
textview.answer, textview.answer text { background-color: transparent; font-size: 14px; }
label.answer { font-size: 14px; }

/* A turn of the conversation that is already answered. */
.past-turn {
  border-left: 2px solid alpha(#ffffff, 0.14);
  padding-left: 12px;
  opacity: 0.85;
}
.turn-question { font-weight: 700; }

.promise image { color: @success_color; }
.promise image.warning { color: @warning_color; }
"#
);

const LIGHT_CSS: &str = concat!(
    include_str!("../../../../data/raven-glass-light.css"),
    ".command, .text-well { background-color: alpha(#000000, 0.05); border-color: alpha(#000000, 0.08); }\n",
    ".past-turn { border-left-color: alpha(#000000, 0.14); }\n",
    "button.metric-card:hover { background-color: alpha(#ffffff, 0.95); border-color: alpha(#000000, 0.10); }\n"
);

thread_local! {
    /// The accent and light-mode provider, replaced (never stacked) on every
    /// change to the desktop's appearance.
    static OVERRIDES: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    /// Kept alive for as long as the app runs; dropping it stops the watch.
    static DESKTOP_MONITOR: RefCell<Option<gio::FileMonitor>> = const { RefCell::new(None) };
}

/// How long `desktop.toml` has to stay quiet before it is read again: one
/// save from Settings arrives as a burst of events.
const DESKTOP_SETTLE: Duration = Duration::from_millis(150);

/// The shared sheet and Oracle's classes in one provider; the accent and the
/// light-mode overrides in a second one above it, exactly as the other Raven
/// apps layer theirs. The second follows `desktop.toml` from here on.
pub fn load(desktop: &Desktop) {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let base = gtk::CssProvider::new();
    base.load_from_string(BASE_CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &base,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    apply(desktop);
    watch_desktop();
}

/// Light or dark, the accent, and glass on the open windows.
fn apply(desktop: &Desktop) {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let look = &desktop.appearance;
    adw::StyleManager::default().set_color_scheme(match look.theme_mode {
        ThemeMode::Dark => adw::ColorScheme::ForceDark,
        ThemeMode::Light => adw::ColorScheme::ForceLight,
        ThemeMode::Auto => adw::ColorScheme::PreferDark,
    });
    let accent = desktop.accent();
    let mut css =
        format!("@define-color accent_bg_color {accent};\n@define-color accent_color {accent};\n");
    if look.theme_mode == ThemeMode::Light {
        css.push_str(LIGHT_CSS);
    }
    OVERRIDES.with(|slot| {
        if let Some(old) = slot.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        let overrides = gtk::CssProvider::new();
        overrides.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &display,
            &overrides,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        *slot.borrow_mut() = Some(overrides);
    });

    // Glass is the material of a Raven window of its own; dialogs over one
    // stay opaque.
    let toplevels = gtk::Window::toplevels();
    for i in 0..toplevels.n_items() {
        if let Some(window) = toplevels.item(i).and_downcast::<gtk::Window>() {
            if window.has_css_class("raven") && window.transient_for().is_none() {
                set_glass(&window, look.transparency);
            }
        }
    }
}

/// Follow Settings: re-read `desktop.toml` whenever it changes. The directory
/// is watched, not the file, because Settings replaces the file by rename and
/// it may not exist yet.
fn watch_desktop() {
    let path = Desktop::path();
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let name = name.to_os_string();
    let Ok(monitor) = gio::File::for_path(dir)
        .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
    else {
        return;
    };
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    monitor.connect_changed(move |_, file, other, event| {
        if matches!(
            event,
            gio::FileMonitorEvent::AttributeChanged
                | gio::FileMonitorEvent::PreUnmount
                | gio::FileMonitorEvent::Unmounted
        ) {
            return;
        }
        let names_desktop = |f: Option<&gio::File>| {
            f.and_then(|f| f.basename())
                .is_some_and(|b| b.as_os_str() == name.as_os_str())
        };
        if !names_desktop(Some(file)) && !names_desktop(other) {
            return;
        }
        if let Some(id) = pending.borrow_mut().take() {
            id.remove();
        }
        let fired = pending.clone();
        let id = glib::timeout_add_local_once(DESKTOP_SETTLE, move || {
            fired.borrow_mut().take();
            apply(&Desktop::load());
        });
        *pending.borrow_mut() = Some(id);
    });
    DESKTOP_MONITOR.with(|m| *m.borrow_mut() = Some(monitor));
}

/// Alpha only; the blur behind a glass window is the compositor's.
pub fn set_glass(window: &impl IsA<gtk::Widget>, on: bool) {
    if on {
        window.add_css_class("glass");
    } else {
        window.remove_css_class("glass");
    }
}
