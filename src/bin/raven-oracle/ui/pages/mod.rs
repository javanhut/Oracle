//! The pages, in sidebar order.

pub mod ask;
pub mod context;
pub mod explain;
pub mod findings;
pub mod overview;
pub mod settings;

use std::rc::Rc;

use gtk4 as gtk;

use super::App;

#[derive(Clone)]
pub struct PageInfo {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub build: fn(&Rc<App>) -> gtk::Widget,
    /// Settings sits below a separator.
    pub separated: bool,
    /// The colour of the icon tile beside the title in the sidebar; see
    /// `.nav-icon` in `data/raven-glass.css`. Names a domain, never the
    /// accent, so the sidebar stays legible under any accent.
    pub tint: &'static str,
}

pub fn all() -> Vec<PageInfo> {
    vec![
        PageInfo {
            id: "overview",
            title: "Overview",
            icon: "view-grid-symbolic",
            build: overview::build,
            separated: false,
            tint: "blue",
        },
        PageInfo {
            id: "findings",
            title: "Findings",
            icon: "dialog-warning-symbolic",
            build: findings::build,
            separated: false,
            tint: "orange",
        },
        PageInfo {
            id: "ask",
            title: "Ask",
            icon: "dialog-question-symbolic",
            build: ask::build,
            separated: false,
            tint: "purple",
        },
        PageInfo {
            id: "explain",
            title: "Explain an Error",
            icon: "utilities-terminal-symbolic",
            build: explain::build,
            separated: false,
            tint: "teal",
        },
        PageInfo {
            id: "context",
            title: "What a Model Sees",
            icon: "view-reveal-symbolic",
            build: context::build,
            separated: false,
            tint: "green",
        },
        PageInfo {
            id: "settings",
            title: "Settings",
            icon: "emblem-system-symbolic",
            build: settings::build,
            separated: true,
            tint: "graphite",
        },
    ]
}

pub fn ids() -> Vec<&'static str> {
    all().iter().map(|p| p.id).collect()
}
