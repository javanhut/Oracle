//! The look: Raven Glass, the stylesheet shared with Settings, Store and Power
//! (`data/raven-glass.css`, kept identical across the repos), plus the classes
//! only Oracle draws -- the verdict hero, severity colours, command wells.

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
    ".past-turn { border-left-color: alpha(#000000, 0.14); }\n"
);

/// The shared sheet and Oracle's classes in one provider; the accent and the
/// light-mode overrides in a second one above it, exactly as the other Raven
/// apps layer theirs.
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
    let overrides = gtk::CssProvider::new();
    overrides.load_from_string(&css);
    gtk::style_context_add_provider_for_display(
        &display,
        &overrides,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
}

/// Alpha only; the blur behind a glass window is the compositor's.
pub fn set_glass(window: &adw::ApplicationWindow, on: bool) {
    if on {
        window.add_css_class("glass");
    } else {
        window.remove_css_class("glass");
    }
}
