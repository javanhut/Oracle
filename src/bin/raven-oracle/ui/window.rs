//! The shell of the window: the sidebar with the sections, a header that
//! names the page, and a stack of pages. The same bones as Raven Store and
//! Raven Settings, so Oracle reads as part of the same desktop.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use oracle::config::Config;
use oracle::diagnose;

use super::pages::{self, PageInfo};
use super::{App, Nav, state, theme, widgets};
use crate::desktop::Desktop;

pub fn build(gtk_app: &adw::Application, app: &Rc<App>) -> (adw::ApplicationWindow, Nav) {
    let window = adw::ApplicationWindow::builder()
        .application(gtk_app)
        .title("Raven Oracle")
        .default_width(1180)
        .default_height(760)
        .build();
    window.add_css_class("raven");

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .hexpand(true)
        .vexpand(true)
        .build();

    // ---- sidebar --------------------------------------------------------
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 10);
    sidebar.add_css_class("sidebar");
    sidebar.append(&brand());

    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_vexpand(true);

    let mut rows: Vec<Option<&'static str>> = Vec::new();
    for info in pages::all() {
        if info.separated {
            let sep = gtk::ListBoxRow::builder()
                .child(&gtk::Separator::new(gtk::Orientation::Horizontal))
                .selectable(false)
                .activatable(false)
                .build();
            sep.set_sensitive(false);
            sep.add_css_class("separator-row");
            list.append(&sep);
            rows.push(None);
        }
        list.append(&nav_row(&info));
        rows.push(Some(info.id));
        stack.add_named(&(info.build)(app), Some(info.id));
    }
    sidebar.append(&list);
    sidebar.append(&status_card(app));

    {
        let stack = stack.clone();
        let rows = rows.clone();
        list.connect_row_selected(move |_, row| {
            if let Some(row) = row
                && let Some(Some(id)) = rows.get(row.index() as usize)
            {
                stack.set_visible_child_name(id);
            }
        });
    }
    list.select_row(list.row_at_index(0).as_ref());

    // ---- header ---------------------------------------------------------
    // The header names the page, bound to the stack so a page opened from a
    // button rather than the sidebar is named too.
    let title = adw::WindowTitle::new("Overview", "");
    {
        let title = title.clone();
        stack.connect_visible_child_name_notify(move |stack| {
            let name = stack.visible_child_name().unwrap_or_default();
            let heading = pages::all()
                .into_iter()
                .find(|p| p.id == name.as_str())
                .map(|p| p.title)
                .unwrap_or("Raven Oracle");
            title.set_title(heading);
        });
    }
    let header = adw::HeaderBar::builder()
        .title_widget(&title)
        .show_title(true)
        .build();
    let show_sidebar = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Sections")
        .visible(false)
        .build();
    header.pack_start(&show_sidebar);
    let recheck = gtk::Button::from_icon_name("view-refresh-symbolic");
    recheck.set_tooltip_text(Some("Check the machine again (Ctrl+R)"));
    {
        let app = app.clone();
        recheck.connect_clicked(move |_| app.rescan());
    }
    header.pack_end(&recheck);
    app.on_change(move |app| recheck.set_sensitive(!app.state.borrow().scanning));

    let toolbar = adw::ToolbarView::new();
    toolbar.set_top_bar_style(adw::ToolbarStyle::Raised);
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));

    let sidebar_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(false)
        .child(&sidebar)
        .build();
    let split = adw::OverlaySplitView::builder()
        .sidebar(&sidebar_scroller)
        .content(&toolbar)
        .sidebar_width_fraction(0.22)
        .min_sidebar_width(220.0)
        .max_sidebar_width(260.0)
        .build();
    split
        .bind_property("show-sidebar", &show_sidebar, "active")
        .bidirectional()
        .sync_create()
        .build();

    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        900.0,
        adw::LengthUnit::Px,
    ));
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    narrow.add_setter(&show_sidebar, "visible", Some(&true.to_value()));
    {
        let stack = stack.clone();
        narrow.connect_apply(move |_| widgets::set_columns_stacked(&stack, true));
    }
    {
        let stack = stack.clone();
        narrow.connect_unapply(move |_| widgets::set_columns_stacked(&stack, false));
    }
    window.add_breakpoint(narrow);
    {
        let split = split.clone();
        list.connect_row_activated(move |_, _| {
            if split.is_collapsed() {
                split.set_show_sidebar(false);
            }
        });
    }

    app.toasts.set_child(Some(&split));
    window.set_content(Some(&app.toasts));
    window.set_size_request(520, 380);

    // Ctrl+R and F5 check again, as refresh does everywhere else.
    let shortcuts = gtk::ShortcutController::new();
    shortcuts.set_scope(gtk::ShortcutScope::Global);
    for trigger in ["<Control>r", "F5"] {
        let app = app.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(trigger),
            Some(gtk::CallbackAction::new(move |_, _| {
                app.rescan();
                glib::Propagation::Stop
            })),
        ));
    }
    window.add_controller(shortcuts);

    // Closing the window mid-answer hides it instead, so a slow model's answer
    // is not thrown away. With nothing coming, closing ends the app as before.
    {
        let app = app.clone();
        window.connect_close_request(move |_| {
            if !app.is_answering() {
                return glib::Propagation::Proceed;
            }
            app.hide_while_answering();
            glib::Propagation::Stop
        });
    }

    (window, Nav { list, stack, rows })
}

fn brand() -> gtk::Box {
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bx.add_css_class("brand");
    // The Raven mark, as /etc/os-release names it, with the app's own icon and
    // then a stock one as the fallbacks for a system without it.
    let icon = gtk::Image::from_icon_name("system-search-symbolic");
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        for name in ["raven-logo", super::APP_ID] {
            if theme.has_icon(name) {
                icon.set_icon_name(Some(name));
                break;
            }
        }
    }
    bx.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_valign(gtk::Align::Center);
    let t = gtk::Label::new(Some("Raven Oracle"));
    t.add_css_class("app-title");
    t.set_xalign(0.0);
    text.append(&t);
    let s = gtk::Label::new(Some("Troubleshooting"));
    s.add_css_class("app-subtitle");
    s.set_xalign(0.0);
    text.append(&s);
    bx.append(&text);
    bx
}

fn nav_row(info: &PageInfo) -> gtk::ListBoxRow {
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let tile = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tile.add_css_class("nav-icon");
    tile.add_css_class(info.tint);
    tile.set_halign(gtk::Align::Center);
    tile.set_valign(gtk::Align::Center);
    // The icon fills the tile so it can sit in its centre. Setting the tile's
    // own hexpand explicitly stops that expansion propagating up and pushing
    // the label into the middle of the row.
    tile.set_hexpand(false);
    let icon = gtk::Image::from_icon_name(info.icon);
    icon.set_hexpand(true);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    tile.append(&icon);
    bx.append(&tile);
    let l = gtk::Label::new(Some(info.title));
    l.set_xalign(0.0);
    l.set_hexpand(true);
    bx.append(&l);
    gtk::ListBoxRow::builder().child(&bx).build()
}

/// The verdict at the foot of the sidebar; clicking opens Findings.
fn status_card(app: &Rc<App>) -> gtk::Button {
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let icon = gtk::Image::from_icon_name("content-loading-symbolic");
    bx.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    text.set_hexpand(true);
    let t = gtk::Label::new(Some("Checking this machine…"));
    t.set_xalign(0.0);
    t.add_css_class("name");
    t.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&t);
    let s = gtk::Label::new(None);
    s.set_xalign(0.0);
    s.add_css_class("dim");
    s.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&s);
    bx.append(&text);
    bx.append(&gtk::Image::from_icon_name("go-next-symbolic"));
    let button = gtk::Button::builder().child(&bx).build();
    button.add_css_class("raven-card");
    button.add_css_class("status-card");
    button.add_css_class("flat");
    {
        let app = app.clone();
        button.connect_clicked(move |_| app.navigate("findings"));
    }
    app.on_change(move |app| {
        let st = app.state.borrow();
        if !st.scanned_once {
            t.set_text("Checking this machine…");
            s.set_text("");
            icon.set_icon_name(Some("content-loading-symbolic"));
            return;
        }
        let worst = st.worst();
        let verdict = diagnose::summarise(&st.findings);
        t.set_text(&verdict);
        t.set_tooltip_text(Some(&verdict));
        icon.set_icon_name(Some(state::severity_icon(worst)));
        widgets::set_one_class(
            &icon,
            &state::SEVERITY_CLASSES,
            state::severity_class(worst),
        );
        if st.scanning {
            s.set_text("Checking again…");
        } else {
            s.set_text(&format!("Checked at {}", st.checked_at));
        }
    });
    button
}

/// The window Oracle opens instead when its settings file does not parse.
///
/// The command line refuses to run in this state rather than quietly using
/// the defaults, because that would ignore a preference somebody wrote down
/// and believed. The app does the same, and says how to get out of it.
pub fn config_error(
    gtk_app: &adw::Application,
    desktop: Desktop,
    error: &str,
    start_page: &'static str,
) {
    let window = adw::ApplicationWindow::builder()
        .application(gtk_app)
        .title("Raven Oracle")
        .default_width(720)
        .default_height(520)
        .build();
    window.add_css_class("raven");
    theme::set_glass(&window, desktop.appearance.transparency);

    let describe = |e: &str| {
        widgets::escape(&format!(
            "{e}\n\nFix the file, or delete it to go back to the defaults. Oracle will not \
             guess what a file it cannot read meant."
        ))
    };
    let page = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title("Oracle's settings file is not valid")
        .description(describe(error))
        .build();
    let again = gtk::Button::with_label("Try again");
    again.add_css_class("suggested-action");
    again.add_css_class("pill");
    again.set_halign(gtk::Align::Center);
    page.set_child(Some(&again));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    window.set_content(Some(&toolbar));

    {
        let gtk_app = gtk_app.clone();
        let window = window.clone();
        let page = page.clone();
        again.connect_clicked(move |_| match Config::load() {
            Ok(cfg) => {
                // Open the real window before closing this one, or the
                // application exits with its last window.
                super::open(&gtk_app, &desktop, cfg, start_page);
                window.close();
            }
            Err(e) => page.set_description(Some(&describe(&e))),
        });
    }
    window.present();
}
