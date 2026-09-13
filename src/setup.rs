//! The opt-in wizard.
//!
//! This is the only place Oracle writes a config file, and it only runs when
//! somebody types `oracle setup`. It does not run on first use, it is not
//! offered at the end of other commands, and nothing in the system invokes it.
//!
//! What it will not do is as important as what it does. It does not install an
//! inference server, it does not pull a language model, and it does not add an
//! autostart entry, a service file, or a desktop launcher. It writes one TOML
//! file under `~/.config/raven/` and tells you what it found.

use crate::config::Config;
use crate::stt::{self, Voice, WHISPER_MODELS};
use crate::ui;

pub fn run(cfg: &mut Config) -> i32 {
    ui::heading("Oracle setup");
    ui::info(
        "Oracle is a local troubleshooting helper. It runs when you run it, reads your system, \
         and suggests things. It never changes anything by itself.",
    );
    ui::info("");
    ui::info(&format!(
        "This writes one file: {}. Nothing else on the system is touched.",
        crate::config::tilde(&Config::path())
    ));

    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        ui::error("setup needs a terminal to ask questions.");
        ui::note(&format!(
            "You can also just write {} by hand; `oracle status` will tell you what it made of it.",
            crate::config::tilde(&Config::path())
        ));
        return 1;
    }

    choose_model(cfg);
    choose_voice(cfg);
    choose_privacy(cfg);

    ui::heading("Saving");
    match cfg.save() {
        Ok(path) => {
            ui::info(&format!("Written to {}", crate::config::tilde(&path)));
            ui::info("");
            ui::info("Try:");
            ui::command("oracle doctor");
            ui::command("oracle ask \"why is my wifi dropping\"");
            ui::info("");
            ui::note(
                "To undo all of this: delete that file, and remove the binary. Oracle leaves \
                 nothing else behind.",
            );
            0
        }
        Err(e) => {
            ui::error(&e);
            1
        }
    }
}

fn choose_model(cfg: &mut Config) {
    ui::heading("Local model");
    ui::info(
        "Oracle can answer questions in plain language if there is a model server on this \
         machine. Without one, the built-in checks still work.",
    );

    // Look before asking: telling someone what is already running beats making
    // them guess which backend they installed six months ago.
    let mut found: Vec<(&str, String)> = Vec::new();
    for (backend, endpoint) in [
        ("ollama", "http://127.0.0.1:11434"),
        ("llamacpp", "http://127.0.0.1:8080"),
    ] {
        let mut probe = cfg.clone();
        probe.model.backend = backend.into();
        probe.model.endpoint = endpoint.into();
        probe.model.name = String::new();
        if let Ok(b) = crate::model::backend_for(&probe) {
            let a = b.availability();
            if a.reachable {
                found.push((backend, endpoint.to_string()));
                ui::info(&format!(
                    "  {} a {backend} server is running at {endpoint} ({} model{})",
                    ui::green("found"),
                    a.models.len(),
                    if a.models.len() == 1 { "" } else { "s" }
                ));
            }
        }
    }

    if found.is_empty() {
        ui::info(&format!(
            "  {} no local model server is running.",
            ui::dim("none")
        ));
        ui::info("");
        ui::info("  Oracle does not install one for you. If you want prose answers:");
        ui::info("");
        ui::command("rvn install ollama");
        ui::command("ollama serve &");
        ui::command("ollama pull qwen2.5:3b-instruct");
        ui::info("");
        ui::info(&format!(
            "  On this machine ({} of RAM), a 3B instruct model is the comfortable size.",
            ui::bytes(crate::sys::meminfo("MemTotal").unwrap_or(0))
        ));
        ui::info("");

        if ui::confirm("  Set Oracle up to use ollama anyway, for when you do install it?") {
            cfg.model.backend = "ollama".into();
            cfg.model.endpoint = "http://127.0.0.1:11434".into();
        } else {
            cfg.model.backend = "none".into();
            ui::note(
                "  Checks only. `oracle doctor` works; `oracle ask` will say it has no model.",
            );
        }
        return;
    }

    let (backend, endpoint) = if found.len() == 1 {
        found[0].clone()
    } else {
        ui::info("");
        for (i, (b, e)) in found.iter().enumerate() {
            ui::info(&format!("  {}. {b} at {e}", i + 1));
        }
        let pick = ui::prompt_line("  Which one? [1] ").unwrap_or_default();
        let idx = pick.trim().parse::<usize>().unwrap_or(1).saturating_sub(1);
        found.get(idx).cloned().unwrap_or_else(|| found[0].clone())
    };

    cfg.model.backend = backend.to_string();
    cfg.model.endpoint = endpoint;

    // Offer the models that are actually installed, and let the default stand.
    if let Ok(b) = crate::model::backend_for(cfg) {
        let a = b.availability();
        if !a.models.is_empty() {
            ui::info("");
            ui::info("  Models on that server:");
            for m in &a.models {
                let mark = if Some(m) == a.selected.as_ref() {
                    ui::green(" <- would be used")
                } else {
                    String::new()
                };
                ui::info(&format!("    {m}{mark}"));
            }
            if let Some(name) =
                ui::prompt_line("  Use a specific one? [Enter to keep the default] ")
                && !name.trim().is_empty()
            {
                cfg.model.name = name.trim().to_string();
            }
        }
    }
}

fn choose_voice(cfg: &mut Config) {
    ui::heading("Dictation (optional)");
    ui::info(
        "Oracle can take a question by voice instead of typing. It records only when you run \
         `oracle listen` or `oracle dictate`, shows you the transcript, and deletes the audio \
         immediately.",
    );
    ui::info("There is no hotword and no background listening. That is not a setting.");
    ui::info("");

    if !ui::confirm("  Set up dictation?") {
        cfg.voice.enabled = false;
        ui::note("  Left off. `oracle voice setup` turns it on later.");
        return;
    }

    let mut probe = cfg.voice.clone();
    probe.enabled = true;
    match Voice::resolve_ignoring_enabled(&probe) {
        Ok(v) => {
            cfg.voice.enabled = true;
            ui::info(&format!("  {} {}", ui::green("ready"), v.describe()));
        }
        Err(e) => {
            ui::info(&format!("  {} {e}", ui::yellow("not ready")));
            ui::info("");
            print_voice_install_help();
            cfg.voice.enabled = false;
            ui::note("  Left off until the pieces are there. Run `oracle voice setup` again then.");
        }
    }
}

/// What someone needs to install for dictation, stated once.
pub fn print_voice_install_help() {
    ui::info("  Dictation needs two things:");
    ui::info("");
    ui::info("  1. whisper.cpp, which provides the transcriber:");
    ui::command("rvn install whisper.cpp");
    ui::info("");
    ui::info("  2. A model file. `oracle voice models` lists them; the usual choice is:");
    ui::command("oracle voice fetch base.en");
    ui::info("");
    ui::info(&format!(
        "  Models are kept in {}.",
        crate::config::tilde(&stt::models_dir())
    ));
    ui::info(&format!(
        "  On this machine, {} is the size that stays comfortable.",
        WHISPER_MODELS[1].name
    ));
}

fn choose_privacy(cfg: &mut Config) {
    ui::heading("Privacy");
    ui::info(&format!(
        "  Redaction is {}. System details are scrubbed of keys, passphrases, MAC addresses, \
         public IPs and your username before anything is shown to a model.",
        if cfg.privacy.redact {
            ui::green("on")
        } else {
            ui::yellow("off")
        }
    ));
    ui::info("  The model endpoint must be on this machine. A remote one is refused unless you");
    ui::info("  deliberately turn that off in the config file.");
    ui::info(&format!(
        "  History is {}: Oracle does not keep a record of what you asked.",
        if cfg.privacy.keep_history {
            ui::yellow("on")
        } else {
            ui::green("off")
        }
    ));
    ui::info("");
    ui::info("  See exactly what would be sent, any time:");
    ui::command("oracle context");
}

/// Download a whisper model, on an explicit request, with the size stated
/// first.
///
/// `curl` does the transfer rather than a built-in HTTP client, for two
/// reasons: Oracle's own client cannot do TLS on purpose, and using the
/// system's downloader keeps the user's proxy settings, certificates and
/// bandwidth limits in play.
pub fn fetch_whisper_model(name: &str, assume_yes: bool) -> i32 {
    let Some(model) = WHISPER_MODELS.iter().find(|m| m.name == name) else {
        ui::error(&format!("unknown model {name:?}"));
        ui::info("Known models:");
        for m in WHISPER_MODELS {
            ui::info(&format!("  {:<16} {:>8}  {}", m.name, m.size, m.note));
        }
        return 1;
    };

    let dir = stt::models_dir();
    let dest = dir.join(model.file);

    if dest.exists() {
        ui::info(&format!(
            "{} is already at {}",
            model.name,
            crate::config::tilde(&dest)
        ));
        return 0;
    }

    let url = stt::model_url(model.file);
    ui::info(&format!("Model:       {} ({})", model.name, model.size));
    ui::info(&format!("From:        {url}"));
    ui::info(&format!("To:          {}", crate::config::tilde(&dest)));
    ui::info("");
    ui::info(
        "This is the only time Oracle downloads anything, and only because you asked for it \
         by name.",
    );

    if !assume_yes && !ui::confirm("Download it?") {
        ui::info("Nothing downloaded.");
        return 0;
    }

    if !crate::sys::have("curl") {
        ui::error("curl is not installed, and Oracle does not implement its own downloader.");
        ui::info("Fetch it yourself with anything you like:");
        ui::command(&format!("mkdir -p {}", dir.display()));
        ui::command(&format!("wget -O {} {url}", dest.display()));
        return 1;
    }

    if let Err(e) = std::fs::create_dir_all(&dir) {
        ui::error(&format!("cannot create {}: {e}", dir.display()));
        return 1;
    }

    // Download beside the target and rename, so an interrupted transfer never
    // leaves a truncated file that looks like a working model.
    let partial = dest.with_extension("bin.part");
    let status = std::process::Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--progress-bar",
            "--continue-at",
            "-",
            "--output",
            &partial.to_string_lossy(),
            &url,
        ])
        .status();

    match status {
        Ok(s) if s.success() => match std::fs::rename(&partial, &dest) {
            Ok(()) => {
                ui::info(&format!("Saved to {}", crate::config::tilde(&dest)));
                ui::note("`oracle voice setup` will find it automatically.");
                0
            }
            Err(e) => {
                ui::error(&format!("downloaded, but could not move into place: {e}"));
                1
            }
        },
        Ok(_) => {
            let _ = std::fs::remove_file(&partial);
            ui::error("the download failed");
            1
        }
        Err(e) => {
            ui::error(&format!("could not run curl: {e}"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetching_an_unknown_model_fails_and_lists_the_real_ones() {
        crate::ui::init(true);
        crate::ui::set_quiet(true);
        let code = fetch_whisper_model("not-a-model", true);
        crate::ui::set_quiet(false);
        assert_eq!(code, 1);
    }

    #[test]
    fn every_listed_model_has_a_size_and_a_reason_to_pick_it() {
        for m in WHISPER_MODELS {
            assert!(!m.size.is_empty());
            assert!(m.note.len() > 20, "{} needs a real note", m.name);
        }
    }
}
