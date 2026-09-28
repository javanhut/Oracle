//! Raven Oracle -- Oracle's desktop app.
//!
//! The same checks, findings and local model as the `oracle` command, in a
//! window that looks like the rest of Raven. It holds to the rules the command
//! line does: it checks the machine when it opens and when asked, never on a
//! timer; it never runs a suggested command, only copies one; and it talks to
//! a model on this machine or to none. Closing the window ends it -- unless an
//! answer is still coming, in which case the window hides until that answer
//! is ready, says so with a notification, and the app ends once it has been
//! seen. There is no tray icon, no autostart entry, and nothing that starts it
//! without being opened.

mod desktop;
mod ui;

const HELP: &str = "\
raven-oracle -- Oracle's desktop app

USAGE
  raven-oracle [--page <id>]

PAGES
  overview, findings, ask, explain, context, settings

The command line is `oracle`; `oracle help` has the rest.
";

fn main() -> glib::ExitCode {
    let mut start_page = "overview";
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("raven-oracle {}", env!("CARGO_PKG_VERSION"));
                return glib::ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                print!("{HELP}");
                return glib::ExitCode::SUCCESS;
            }
            "--page" => match args.next() {
                Some(p) => match ui::pages::ids().into_iter().find(|id| *id == p) {
                    Some(id) => start_page = id,
                    None => eprintln!("raven-oracle: there is no page called {p:?}"),
                },
                None => eprintln!("raven-oracle: --page needs a name"),
            },
            other => eprintln!("raven-oracle: ignoring unknown argument {other}"),
        }
    }
    ui::run(start_page)
}
