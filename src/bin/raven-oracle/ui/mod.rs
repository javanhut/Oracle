//! The GTK side. Everything here runs on the main thread; work that blocks --
//! the probes, the model -- runs on a worker and comes back through a closure
//! on the main loop, the way Raven Store does it.
//!
//! The window holds to Oracle's rules. It checks the machine when it opens
//! and when asked, never on a timer. It never runs a suggested command; it
//! copies one. It talks to a model on this machine or to none.

pub mod answer;
pub mod notify;
pub mod pages;
pub mod state;
pub mod theme;
pub mod widgets;
pub mod window;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use oracle::config::{self, Config};
use oracle::probe::{self, Finding, ProbeOptions, SystemView};
use oracle::{diagnose, model, prompt, sys};

use crate::desktop::Desktop;
use state::{Filter, Host, ModelState, State};

pub const APP_ID: &str = "com.ravenoracle.Raven";

type Listener = Box<dyn Fn(&Rc<App>)>;

/// The sidebar and the page stack, and which page each sidebar row opens.
pub struct Nav {
    pub list: gtk::ListBox,
    pub stack: gtk::Stack,
    /// One entry per sidebar row; `None` for a separator.
    pub rows: Vec<Option<&'static str>>,
}

pub struct App {
    pub state: RefCell<State>,
    pub filter: Cell<Filter>,
    pub toasts: adw::ToastOverlay,
    window: RefCell<Option<adw::ApplicationWindow>>,
    listeners: RefCell<Vec<Listener>>,
    nav: RefCell<Option<Nav>>,
    /// The Ask page's conversation, so a finding can be explained from the
    /// Findings page.
    pub ask_conversation: RefCell<Option<Rc<answer::Conversation>>>,
    /// How many answers are still coming, across Ask and Explain.
    answering: Cell<usize>,
    notifier: Rc<notify::Notifier>,
}

impl App {
    pub fn window(&self) -> adw::ApplicationWindow {
        self.window.borrow().clone().expect("window not built yet")
    }

    pub fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    pub fn on_change(&self, f: impl Fn(&Rc<App>) + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    pub fn notify(self: &Rc<Self>) {
        for l in self.listeners.borrow().iter() {
            l(self);
        }
    }

    pub fn navigate(&self, id: &str) {
        if let Some(nav) = self.nav.borrow().as_ref() {
            if let Some(i) = nav.rows.iter().position(|r| *r == Some(id)) {
                nav.list
                    .select_row(nav.list.row_at_index(i as i32).as_ref());
            }
            if nav.stack.child_by_name(id).is_some() {
                nav.stack.set_visible_child_name(id);
            }
        }
    }

    /// The page on screen, as one of `pages::ids`.
    fn visible_page(&self) -> Option<&'static str> {
        let nav = self.nav.borrow();
        let name = nav.as_ref()?.stack.visible_child_name()?;
        pages::ids().into_iter().find(|id| *id == name.as_str())
    }

    /// Whether any answer is still coming.
    pub fn is_answering(&self) -> bool {
        self.answering.get() > 0
    }

    pub fn answer_began(&self) {
        self.answering.set(self.answering.get() + 1);
    }

    /// An answer on `page` ended. `report` is what to say about it, or `None`
    /// when the person stopped it and so already knows.
    pub fn answer_ended(self: &Rc<Self>, page: &'static str, report: Option<notify::Report>) {
        self.answering.set(self.answering.get().saturating_sub(1));
        let Some(report) = report else {
            return;
        };
        let window = self.window();
        let on_page = self.visible_page() == Some(page);
        if !notify::worth_notifying(window.is_visible(), window.is_active(), on_page) {
            return;
        }
        let app = self.clone();
        self.notifier.send(&report, move |event| {
            app.on_notification(notify::Kind::Ready, page, event)
        });
    }

    /// The window was closed while an answer was still coming. It is hidden
    /// rather than ended, and comes back through the notification when the
    /// answer is ready.
    pub fn hide_while_answering(self: &Rc<Self>) {
        let window = self.window();
        let page = self.visible_page().unwrap_or("ask");
        window.set_visible(false);
        let app = self.clone();
        self.notifier.send(&notify::working(), move |event| {
            app.on_notification(notify::Kind::Working, page, event)
        });
    }

    fn on_notification(
        self: &Rc<Self>,
        kind: notify::Kind,
        page: &'static str,
        event: notify::Event,
    ) {
        let window = self.window();
        match notify::respond(kind, event, !window.is_visible(), self.is_answering()) {
            notify::Response::Show => {
                window.present();
                self.navigate(page);
            }
            // Nothing is coming, so the close request goes through and the
            // app ends with its window.
            notify::Response::Quit => window.close(),
            notify::Response::Nothing => {}
        }
    }

    pub fn set_filter(self: &Rc<Self>, filter: Filter) {
        if self.filter.get() != filter {
            self.filter.set(filter);
            self.notify();
        }
    }

    /// Put text on the clipboard. The furthest Oracle ever goes towards
    /// running a command is handing it to you.
    pub fn copy(&self, text: &str, what: &str) {
        match gtk::gdk::Display::default() {
            Some(display) => {
                display.clipboard().set_text(text);
                self.toast(&format!("Copied {what}"));
            }
            None => self.toast("There is no clipboard to copy to"),
        }
    }

    /// Run the probes and the rules again.
    pub fn rescan(self: &Rc<Self>) {
        let cfg = {
            let mut st = self.state.borrow_mut();
            if st.scanning {
                return;
            }
            st.scanning = true;
            st.config.clone()
        };
        self.notify();
        let app = self.clone();
        spawn(
            move || scan(&cfg),
            move |result| {
                {
                    let mut st = app.state.borrow_mut();
                    st.scanning = false;
                    st.scanned_once = true;
                    st.revision += 1;
                    st.findings = result.findings;
                    st.host = result.host;
                    st.context = result.context;
                    st.checked_at = glib::DateTime::now_local()
                        .ok()
                        .and_then(|d| d.format("%H:%M").ok())
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                }
                app.notify();
            },
        );
    }

    /// Ask the configured server what it has.
    pub fn check_model(self: &Rc<Self>) {
        let cfg = {
            let mut st = self.state.borrow_mut();
            st.model = ModelState::Checking;
            st.config.clone()
        };
        self.notify();
        let app = self.clone();
        spawn(
            move || probe_model(&cfg),
            move |m| {
                app.state.borrow_mut().model = m;
                app.notify();
            },
        );
    }

    /// Ask the model a question about this machine, on the Ask page.
    pub fn ask(self: &Rc<Self>, question: &str) {
        let Some(conversation) = self.ask_conversation.borrow().clone() else {
            return;
        };
        self.navigate("ask");
        let q = question.to_string();
        conversation.start(self, question, question, move |cfg| {
            // Only the parts of the system the question is about, so the
            // model is told no more about the machine than it needs.
            let areas = probe::areas_for_question(&q);
            let view = SystemView::gather_for(cfg, &areas, ProbeOptions::default());
            let findings = diagnose::run(&view);
            prompt::build(&q, &view, &findings, cfg)
        });
    }

    /// Have the model expand on one finding, on the Ask page.
    pub fn explain_finding(self: &Rc<Self>, finding: &Finding) {
        let Some(conversation) = self.ask_conversation.borrow().clone() else {
            return;
        };
        self.navigate("ask");
        let finding = finding.clone();
        let heading = format!("About: {}", finding.title);
        let asked = prompt::finding_as_question(&finding);
        conversation.start(self, &heading, &asked, move |cfg| {
            let view = SystemView::gather(cfg, ProbeOptions::default());
            prompt::build_finding_explanation(&finding, &view, cfg)
        });
    }

    /// Check and write a new config, then look again with it.
    ///
    /// A backend that cannot be built -- an unknown name, or an endpoint off
    /// this machine that has not been permitted -- is refused before anything
    /// is written, the same check every model call makes.
    pub fn save_config(self: &Rc<Self>, cfg: Config) -> Result<PathBuf, String> {
        if let Some(problem) = state::endpoint_problem(&cfg) {
            return Err(problem);
        }
        let backend = cfg.model.backend.trim().to_ascii_lowercase();
        if !backend.is_empty() && backend != "none" {
            model::backend_for(&cfg).map(|_| ())?;
        }
        let path = cfg.save()?;
        let look_again = {
            let st = self.state.borrow();
            state::skipped_areas(&st.config) != state::skipped_areas(&cfg)
                || st.config.privacy.redact != cfg.privacy.redact
        };
        self.state.borrow_mut().config = cfg;
        self.notify();
        self.check_model();
        if look_again {
            self.rescan();
        }
        Ok(path)
    }

    /// Everything Oracle has written on this account.
    pub fn written_files() -> Vec<PathBuf> {
        [Config::path(), config::state_dir()]
            .into_iter()
            .filter(|p| p.exists())
            .collect()
    }

    /// Delete the config and anything downloaded, and go back to the
    /// defaults. The binaries stay; removing those is the package's job.
    pub fn forget(self: &Rc<Self>, targets: &[PathBuf]) -> Result<(), String> {
        let mut failures = Vec::new();
        for t in targets {
            let r = if t.is_dir() {
                std::fs::remove_dir_all(t)
            } else {
                std::fs::remove_file(t)
            };
            if let Err(e) = r {
                failures.push(format!("{}: {e}", config::tilde(t)));
            }
        }
        self.state.borrow_mut().config = Config::default();
        self.notify();
        self.check_model();
        self.rescan();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n"))
        }
    }
}

/// Ask the server named in `cfg` what it has. Blocks on the network, so run
/// it on a worker. Settings uses it on the form's values before they are
/// saved; `App::check_model` uses it on the saved ones.
pub fn probe_model(cfg: &Config) -> ModelState {
    if let Some(problem) = state::endpoint_problem(cfg) {
        return ModelState::Off(problem);
    }
    match model::backend_for(cfg) {
        Ok(backend) => ModelState::Checked {
            backend: backend.name(),
            availability: backend.availability(),
        },
        Err(e) => ModelState::Off(e),
    }
}

struct Scan {
    findings: Vec<Finding>,
    host: Host,
    context: String,
}

fn scan(cfg: &Config) -> Scan {
    let view = SystemView::gather(cfg, ProbeOptions::default());
    let findings = diagnose::run(&view);
    // The same text `oracle context` prints, kept so the page that shows it is
    // showing what this check actually read.
    let context = format!(
        "{}\n{}",
        prompt::system_context(&view, cfg),
        prompt::findings_context(&findings)
    );
    let h = &view.host;
    Scan {
        host: Host {
            distro: h.distro.clone(),
            kernel: h.kernel.clone(),
            init: h.init.clone(),
            uptime: sys::humanise_duration(h.uptime_seconds),
            desktop: h.desktop.clone(),
            root: h.running_as_root,
        },
        findings,
        context,
    }
}

/// Run `work` off the main thread, then `done` with its result on it.
pub fn spawn<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    glib::spawn_future_local(async move {
        match gio::spawn_blocking(work).await {
            Ok(v) => done(v),
            Err(_) => eprintln!("raven-oracle: a background task panicked"),
        }
    });
}

pub fn run(start_page: &'static str) -> glib::ExitCode {
    // `NON_UNIQUE`: a second launch is a second window, as for every Raven
    // app, rather than a raise of the first.
    let gtk_app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    gtk_app.connect_activate(move |gtk_app| {
        let desktop = Desktop::load();
        theme::load(&desktop);
        match Config::load() {
            Ok(cfg) => open(gtk_app, &desktop, cfg, start_page),
            // A file that exists and does not parse is not quietly replaced
            // by the defaults, here or on the command line.
            Err(e) => window::config_error(gtk_app, desktop, &e, start_page),
        }
    });
    gtk_app.run_with_args::<&str>(&[])
}

pub fn open(gtk_app: &adw::Application, desktop: &Desktop, cfg: Config, start_page: &str) {
    let app = Rc::new(App {
        state: RefCell::new(State {
            config: cfg,
            ..Default::default()
        }),
        filter: Cell::new(Filter::All),
        toasts: adw::ToastOverlay::new(),
        window: RefCell::new(None),
        listeners: RefCell::new(Vec::new()),
        nav: RefCell::new(None),
        ask_conversation: RefCell::new(None),
        answering: Cell::new(0),
        notifier: Rc::new(notify::Notifier::default()),
    });

    let (window, nav) = window::build(gtk_app, &app);
    theme::set_glass(&window, desktop.appearance.transparency);
    *app.window.borrow_mut() = Some(window.clone());
    *app.nav.borrow_mut() = Some(nav);
    window.present();
    app.navigate(start_page);

    if sys::is_root() {
        let t = adw::Toast::new(
            "Running as root. Oracle is meant to run as you, and cannot show which suggestions need privilege.",
        );
        t.set_timeout(0);
        app.toasts.add_toast(t);
    }

    app.rescan();
    app.check_model();

    if let Some(dir) = std::env::var_os("RAVEN_ORACLE_SNAPSHOT") {
        snapshot_pages(&app, PathBuf::from(dir));
    }
}

/// Development aid: with `RAVEN_ORACLE_SNAPSHOT=<dir>`, wait for the first
/// check, render every page to a PNG in that directory, and quit. Lets the
/// interface be looked at from a shell, as Raven Store's hook does.
fn snapshot_pages(app: &Rc<App>, dir: PathBuf) {
    let app = app.clone();
    let mut pending: Vec<&'static str> = pages::ids();
    let mut current: Option<&'static str> = None;
    glib::timeout_add_local(std::time::Duration::from_millis(1200), move || {
        if !app.state.borrow().scanned_once {
            return glib::ControlFlow::Continue;
        }
        let window = app.window();
        if let Some(id) = current.take() {
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
            if let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) {
                let _ = std::fs::create_dir_all(&dir);
                let texture = renderer.render_texture(node, None);
                let _ = texture.save_to_png(dir.join(format!("{id}.png")));
            }
        }
        if pending.is_empty() {
            std::process::exit(0);
        }
        let id = pending.remove(0);
        app.navigate(id);
        current = Some(id);
        glib::ControlFlow::Continue
    });
}

/// Ask a yes/no question. `on_answer(true)` when confirmed.
pub fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    action: &str,
    destructive: bool,
    on_answer: impl Fn(bool) + 'static,
) {
    let d = adw::AlertDialog::new(Some(heading), Some(body));
    d.add_response("cancel", "Cancel");
    d.add_response("ok", action);
    d.set_response_appearance(
        "ok",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    d.connect_response(None, move |_, r| on_answer(r == "ok"));
    d.present(Some(parent));
}

/// Tell the person something went wrong, in full.
pub fn alert(parent: &impl IsA<gtk::Widget>, heading: &str, body: &str) {
    let d = adw::AlertDialog::new(Some(heading), Some(body));
    d.add_response("ok", "OK");
    d.set_default_response(Some("ok"));
    d.set_close_response("ok");
    d.present(Some(parent));
}
