//! Dictation. Optional, off by default, and never listening on its own.
//!
//! whisper.cpp is the default transcriber because it is the one that actually
//! fits the situation: a single C++ binary with no runtime, GGML weights from
//! 60MB up, and usable accuracy on a four-core laptop with no GPU. The
//! alternatives each fail one of those tests -- faster-whisper wants a Python
//! and CTranslate2 stack, Vosk is meaningfully less accurate, and the good
//! NVIDIA models want an NVIDIA card. `[voice] command` exists so that
//! anything CLI-shaped can replace it without changing this code.
//!
//! The rules this module holds to:
//!
//! - Nothing records unless a command the user typed asked it to. There is no
//!   hotword, no background listener, and no way to configure one.
//! - Recording is visible while it happens and bounded by `max_seconds`.
//! - The audio file is deleted as soon as it has been transcribed, on every
//!   path including failure.
//! - The transcript is shown to the user before it is used for anything.

use crate::config::{Config, VoiceConfig};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum VoiceError {
    /// Voice is switched off in the config, which is the default state.
    Disabled,
    NoTranscriber(String),
    NoModel(String),
    NoRecorder(String),
    RecordingFailed(String),
    TranscriptionFailed(String),
    Empty,
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VoiceError::Disabled => write!(
                f,
                "dictation is off. Run `oracle voice setup` to turn it on for this account"
            ),
            VoiceError::NoTranscriber(s) => write!(f, "{s}"),
            VoiceError::NoModel(s) => write!(f, "{s}"),
            VoiceError::NoRecorder(s) => write!(f, "{s}"),
            VoiceError::RecordingFailed(s) => write!(f, "recording failed: {s}"),
            VoiceError::TranscriptionFailed(s) => write!(f, "transcription failed: {s}"),
            VoiceError::Empty => write!(f, "nothing was said, or nothing was picked up"),
        }
    }
}

impl std::error::Error for VoiceError {}

/// Everything dictation needs, resolved against this machine.
#[derive(Debug, Clone)]
pub struct Voice {
    pub transcriber: Transcriber,
    pub model: Option<PathBuf>,
    pub recorder: Recorder,
    pub language: String,
    pub max_seconds: u32,
}

#[derive(Debug, Clone)]
pub enum Transcriber {
    /// A whisper.cpp binary, which needs a model file.
    WhisperCpp(PathBuf),
    /// An arbitrary command template containing `{audio}`.
    Command(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorder {
    PipeWire,
    Alsa,
    PulseAudio,
}

impl Recorder {
    pub fn binary(self) -> &'static str {
        match self {
            Recorder::PipeWire => "pw-record",
            Recorder::Alsa => "arecord",
            Recorder::PulseAudio => "parecord",
        }
    }

    /// Arguments to capture 16kHz mono signed-16 PCM, which is what whisper
    /// wants and what every one of these tools can produce.
    fn args(self, out: &str) -> Vec<String> {
        match self {
            Recorder::PipeWire => vec![
                "--rate=16000".into(),
                "--channels=1".into(),
                "--format=s16".into(),
                out.into(),
            ],
            Recorder::Alsa => vec![
                "-q".into(),
                "-f".into(),
                "S16_LE".into(),
                "-r".into(),
                "16000".into(),
                "-c".into(),
                "1".into(),
                "-t".into(),
                "wav".into(),
                out.into(),
            ],
            Recorder::PulseAudio => vec![
                "--rate=16000".into(),
                "--channels=1".into(),
                "--format=s16le".into(),
                "--file-format=wav".into(),
                out.into(),
            ],
        }
    }
}

/// whisper.cpp binaries, newest naming first. Upstream renamed `main` to
/// `whisper-cli`; both are still in the wild.
const WHISPER_BINARIES: [&str; 4] = ["whisper-cli", "whisper-cpp", "whisper", "whisper.cpp"];

/// Where a ggml model might already be, in the order worth looking.
fn model_search_paths() -> Vec<PathBuf> {
    vec![
        crate::config::state_dir().join("models"),
        crate::config::home().join(".local/share/whisper.cpp/models"),
        crate::config::home().join(".cache/whisper"),
        PathBuf::from("/usr/share/whisper.cpp/models"),
        PathBuf::from("/usr/share/whisper.cpp"),
        PathBuf::from("/usr/local/share/whisper.cpp/models"),
        PathBuf::from("/var/lib/whisper"),
    ]
}

impl Voice {
    /// Work out whether dictation can run, without recording anything.
    pub fn resolve(cfg: &Config) -> Result<Voice, VoiceError> {
        let v = &cfg.voice;
        if !v.enabled {
            return Err(VoiceError::Disabled);
        }
        Voice::resolve_ignoring_enabled(v)
    }

    /// The same resolution, skipping the on/off switch. Used by `setup` and
    /// `status`, which need to report what *would* work.
    pub fn resolve_ignoring_enabled(v: &VoiceConfig) -> Result<Voice, VoiceError> {
        let transcriber = find_transcriber(v)?;
        let model = match &transcriber {
            Transcriber::WhisperCpp(_) => Some(find_model(v)?),
            // A custom command is responsible for its own weights.
            Transcriber::Command(_) => None,
        };
        let recorder = find_recorder(v)?;

        Ok(Voice {
            transcriber,
            model,
            recorder,
            language: if v.language.is_empty() {
                "auto".into()
            } else {
                v.language.clone()
            },
            max_seconds: v.max_seconds.clamp(3, 600),
        })
    }

    /// Record, transcribe, and return the text.
    ///
    /// `seconds` fixes the length; `None` records until the user stops it,
    /// which needs a terminal. The audio file is removed before this returns,
    /// whatever happened.
    pub fn capture(&self, seconds: Option<u32>) -> Result<String, VoiceError> {
        let audio = scratch_wav();
        if let Some(dir) = audio.parent() {
            let _ = std::fs::create_dir_all(dir);
        }

        let result = self.record(&audio, seconds).and_then(|_| {
            if !audio.exists() || std::fs::metadata(&audio).map(|m| m.len()).unwrap_or(0) < 1024 {
                return Err(VoiceError::Empty);
            }
            self.transcribe(&audio)
        });

        // The recording is deleted here rather than by the caller so that no
        // future edit to a caller can leave one behind.
        shred(&audio);

        let text = result?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(VoiceError::Empty);
        }
        Ok(text)
    }

    fn record(&self, out: &Path, seconds: Option<u32>) -> Result<(), VoiceError> {
        let bin = self.recorder.binary();
        let args = self.recorder.args(&out.to_string_lossy());
        let limit = seconds.unwrap_or(self.max_seconds).min(self.max_seconds);

        let mut child = Command::new(bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VoiceError::RecordingFailed(format!("could not start {bin}: {e}")))?;

        let interactive = seconds.is_none() && std::io::stdin().is_terminal_hack();

        if interactive {
            crate::ui::note(&format!(
                "Recording. Press Enter to stop, or wait {limit}s."
            ));
            wait_for_enter_or_timeout(&mut child, Duration::from_secs(limit as u64));
        } else {
            crate::ui::note(&format!("Recording for {limit}s."));
            wait_or_timeout(&mut child, Duration::from_secs(limit as u64));
        }

        // SIGINT rather than SIGKILL: every one of these recorders finalises
        // the WAV header on an interrupt, and a killed recorder leaves a file
        // whose declared length is zero, which whisper reads as silence.
        stop_gently(&mut child);

        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            use std::io::Read;
            let _ = e.read_to_string(&mut stderr);
        }
        let _ = child.wait();

        if !out.exists() {
            return Err(VoiceError::RecordingFailed(format!(
                "{bin} produced no file. {}",
                stderr
                    .lines()
                    .next()
                    .unwrap_or("Is a microphone connected?")
            )));
        }
        Ok(())
    }

    fn transcribe(&self, audio: &Path) -> Result<String, VoiceError> {
        match &self.transcriber {
            Transcriber::WhisperCpp(bin) => {
                let model = self
                    .model
                    .as_ref()
                    .ok_or_else(|| VoiceError::NoModel("no whisper model is set".into()))?;

                let threads = std::thread::available_parallelism()
                    .map(|n| n.get().min(8).to_string())
                    .unwrap_or_else(|_| "4".into());

                let mut args: Vec<String> = vec![
                    "-m".into(),
                    model.to_string_lossy().into_owned(),
                    "-f".into(),
                    audio.to_string_lossy().into_owned(),
                    // No timestamps, no progress chatter: stdout should be the
                    // transcript and nothing else.
                    "-nt".into(),
                    "-np".into(),
                    "-t".into(),
                    threads,
                ];
                if self.language != "auto" {
                    args.push("-l".into());
                    args.push(self.language.clone());
                }

                let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let out = crate::sys::run(
                    &bin.to_string_lossy(),
                    &args,
                    // Transcription on a small CPU is genuinely slow; a minute
                    // of audio on a four-core machine can take most of a minute.
                    Duration::from_secs(300),
                )
                .ok_or_else(|| {
                    VoiceError::TranscriptionFailed(format!("could not run {}", bin.display()))
                })?;

                if !out.ok() {
                    let why = out
                        .stderr
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("whisper exited with an error");
                    return Err(VoiceError::TranscriptionFailed(why.trim().to_string()));
                }
                Ok(clean_transcript(&out.stdout))
            }

            Transcriber::Command(template) => {
                let cmd = template.replace("{audio}", &audio.to_string_lossy());
                let out = crate::sys::run("sh", &["-c", &cmd], Duration::from_secs(300))
                    .ok_or_else(|| {
                        VoiceError::TranscriptionFailed(
                            "could not run the transcribe command".into(),
                        )
                    })?;
                if !out.ok() {
                    return Err(VoiceError::TranscriptionFailed(
                        out.stderr.trim().to_string(),
                    ));
                }
                Ok(clean_transcript(&out.stdout))
            }
        }
    }

    pub fn describe(&self) -> String {
        let t = match &self.transcriber {
            Transcriber::WhisperCpp(b) => format!("whisper.cpp ({})", crate::config::tilde(b)),
            Transcriber::Command(c) => format!("custom command ({c})"),
        };
        let m = self
            .model
            .as_ref()
            .map(|p| crate::config::tilde(p))
            .unwrap_or_else(|| "n/a".into());
        format!(
            "{t}, model {m}, recorder {}, language {}",
            self.recorder.binary(),
            self.language
        )
    }
}

fn find_transcriber(v: &VoiceConfig) -> Result<Transcriber, VoiceError> {
    if !v.command.trim().is_empty() {
        if !v.command.contains("{audio}") {
            return Err(VoiceError::NoTranscriber(
                "[voice] command must contain {audio}, which is replaced with the recording's path"
                    .into(),
            ));
        }
        return Ok(Transcriber::Command(v.command.clone()));
    }

    if !v.whisper_bin.trim().is_empty() {
        return match crate::sys::which(&v.whisper_bin) {
            Some(p) => Ok(Transcriber::WhisperCpp(PathBuf::from(p))),
            None => Err(VoiceError::NoTranscriber(format!(
                "the configured whisper binary {} is not there",
                v.whisper_bin
            ))),
        };
    }

    for name in WHISPER_BINARIES {
        if let Some(p) = crate::sys::which(name) {
            return Ok(Transcriber::WhisperCpp(PathBuf::from(p)));
        }
    }

    Err(VoiceError::NoTranscriber(format!(
        "no whisper.cpp binary found. Oracle looked for {} on PATH. Install whisper.cpp, or set \
         [voice] command in {} to any transcriber that takes a WAV and prints text.",
        WHISPER_BINARIES.join(", "),
        crate::config::tilde(&Config::path())
    )))
}

fn find_model(v: &VoiceConfig) -> Result<PathBuf, VoiceError> {
    if !v.model.trim().is_empty() {
        let p = PathBuf::from(expand_tilde(&v.model));
        return if p.is_file() {
            Ok(p)
        } else {
            Err(VoiceError::NoModel(format!(
                "the configured model {} is not there",
                v.model
            )))
        };
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    for dir in model_search_paths() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
            if let Some(name) = name
                && name.starts_with("ggml-")
                && name.ends_with(".bin")
            {
                candidates.push(p);
            }
        }
    }

    if candidates.is_empty() {
        return Err(VoiceError::NoModel(format!(
            "no whisper model found. Oracle looked in {}. `oracle voice models` lists what you \
             can fetch.",
            model_search_paths()
                .iter()
                .map(|p| crate::config::tilde(p))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    // Prefer the best model that is still sensible on a modest CPU. Bigger is
    // more accurate and slower; on four cores, `small` is about the limit of
    // what feels like dictation rather than batch processing.
    let rank = |p: &Path| -> i32 {
        let n = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let base = if n.contains("small") {
            40
        } else if n.contains("base") {
            30
        } else if n.contains("medium") {
            20
        } else if n.contains("tiny") {
            10
        } else if n.contains("large") {
            5
        } else {
            1
        };
        // English-only models are both smaller and better at English.
        base + if n.contains(".en") { 3 } else { 0 }
    };
    candidates.sort_by_key(|p| std::cmp::Reverse(rank(p)));
    Ok(candidates.remove(0))
}

fn find_recorder(v: &VoiceConfig) -> Result<Recorder, VoiceError> {
    let explicit = match v.recorder.trim().to_ascii_lowercase().as_str() {
        "pw-record" | "pipewire" => Some(Recorder::PipeWire),
        "arecord" | "alsa" => Some(Recorder::Alsa),
        "parecord" | "pulse" | "pulseaudio" => Some(Recorder::PulseAudio),
        _ => None,
    };

    if let Some(r) = explicit {
        return if crate::sys::have(r.binary()) {
            Ok(r)
        } else {
            Err(VoiceError::NoRecorder(format!(
                "{} is configured but not installed",
                r.binary()
            )))
        };
    }

    // PipeWire first: it is what a current desktop actually runs, and
    // `pw-record` records without taking the device away from anything else.
    for r in [Recorder::PipeWire, Recorder::PulseAudio, Recorder::Alsa] {
        if crate::sys::have(r.binary()) {
            return Ok(r);
        }
    }

    Err(VoiceError::NoRecorder(
        "no audio recorder found. Oracle can use pw-record (PipeWire), parecord (PulseAudio) or \
         arecord (ALSA); install one of them."
            .into(),
    ))
}

/// Models worth suggesting, with what they cost.
///
/// Sizes are the on-disk size of the GGML file. The recommendation is for a
/// laptop CPU, which is what Raven is usually running on.
pub struct WhisperModel {
    pub name: &'static str,
    pub file: &'static str,
    pub size: &'static str,
    pub note: &'static str,
}

pub const WHISPER_MODELS: [WhisperModel; 5] = [
    WhisperModel {
        name: "tiny.en",
        file: "ggml-tiny.en.bin",
        size: "75 MB",
        note: "Fastest. Fine for short commands, weak on unusual words.",
    },
    WhisperModel {
        name: "base.en",
        file: "ggml-base.en.bin",
        size: "142 MB",
        note: "The sensible default on a laptop CPU. Roughly real time on four cores.",
    },
    WhisperModel {
        name: "small.en",
        file: "ggml-small.en.bin",
        size: "466 MB",
        note: "Noticeably more accurate. Around three times slower than base.",
    },
    WhisperModel {
        name: "medium.en",
        file: "ggml-medium.en.bin",
        size: "1.5 GB",
        note: "Accurate, and slow enough on a CPU that it stops feeling like dictation.",
    },
    WhisperModel {
        name: "large-v3-turbo",
        file: "ggml-large-v3-turbo.bin",
        size: "1.6 GB",
        note: "Best multilingual accuracy. Wants a GPU build to be comfortable.",
    },
];

pub fn model_url(file: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}")
}

pub fn models_dir() -> PathBuf {
    crate::config::state_dir().join("models")
}

fn scratch_wav() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("oracle-dictation-{}.wav", std::process::id()))
}

/// Remove a recording. Best effort by design: if the file cannot be removed,
/// that is worth knowing but is not worth failing a transcription over.
fn shred(path: &Path) {
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

/// Ask a child to stop the way a user pressing Ctrl-C would.
fn stop_gently(child: &mut Child) {
    let pid = child.id().to_string();
    let _ = Command::new("kill")
        .args(["-INT", &pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let _ = child.kill();
}

fn wait_or_timeout(child: &mut Child, limit: Duration) {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait for the user to press Enter, for the recorder to exit, or for the
/// ceiling -- whichever comes first.
fn wait_for_enter_or_timeout(child: &mut Child, limit: Duration) {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = tx.send(());
    });

    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if rx.try_recv().is_ok() {
            return;
        }
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// whisper.cpp prints a little housekeeping even with `-np` on some builds,
/// and wraps long output. This reduces it to the sentence that was said.
fn clean_transcript(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Timestamp-prefixed lines, if -nt was not honoured.
        let line = if line.starts_with('[')
            && let Some(end) = line.find(']')
        {
            line[end + 1..].trim()
        } else {
            line
        };
        // Whisper's own markers for silence and non-speech.
        if line.starts_with("whisper_")
            || line.starts_with("main:")
            || line.starts_with("system_info:")
            || (line.starts_with('(') && line.ends_with(')'))
            || (line.starts_with('[') && line.ends_with(']'))
            || line.starts_with("*")
        {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    out.trim().to_string()
}

/// `IsTerminal` on stdin, wrapped so the recording path reads clearly.
trait TerminalHack {
    fn is_terminal_hack(&self) -> bool;
}

impl TerminalHack for std::io::Stdin {
    fn is_terminal_hack(&self) -> bool {
        use std::io::IsTerminal;
        self.is_terminal()
    }
}

/// Expand a leading `~/` the way a shell would. Config files are hand edited
/// and people write paths that way.
fn expand_tilde(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => crate::config::home()
            .join(rest)
            .to_string_lossy()
            .into_owned(),
        None => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_tilde_in_a_configured_path_is_expanded() {
        let out = expand_tilde("~/models/ggml-base.en.bin");
        let home = crate::config::home().to_string_lossy().into_owned();
        assert!(out.starts_with(&home), "got {out}");
        assert!(!out.contains('~'));
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }

    #[test]
    fn voice_is_off_until_it_is_turned_on() {
        let cfg = Config::default();
        assert!(!cfg.voice.enabled);
        assert!(matches!(Voice::resolve(&cfg), Err(VoiceError::Disabled)));
    }

    #[test]
    fn a_custom_command_must_say_where_the_audio_goes() {
        let mut v = VoiceConfig {
            enabled: true,
            command: "my-transcriber --input file.wav".into(),
            ..Default::default()
        };
        assert!(matches!(
            find_transcriber(&v),
            Err(VoiceError::NoTranscriber(_))
        ));

        v.command = "my-transcriber --input {audio}".into();
        assert!(matches!(find_transcriber(&v), Ok(Transcriber::Command(_))));
    }

    #[test]
    fn recorder_arguments_ask_for_what_whisper_wants() {
        for r in [Recorder::PipeWire, Recorder::Alsa, Recorder::PulseAudio] {
            let args = r.args("/tmp/x.wav").join(" ");
            assert!(
                args.contains("16000"),
                "{} lost the sample rate: {args}",
                r.binary()
            );
            assert!(args.contains("/tmp/x.wav"));
        }
    }

    #[test]
    fn transcripts_lose_whispers_housekeeping_but_keep_the_words() {
        let raw = "\
whisper_init_from_file: loading model
main: processing 'x.wav'

[00:00:00.000 --> 00:00:03.000]   The wifi drops after suspend.
(silence)
";
        assert_eq!(clean_transcript(raw), "The wifi drops after suspend.");
    }

    #[test]
    fn a_multi_line_transcript_becomes_one_line() {
        let raw = "The first part\nand the second part\n";
        assert_eq!(clean_transcript(raw), "The first part and the second part");
    }

    #[test]
    fn an_empty_transcript_stays_empty() {
        assert_eq!(clean_transcript("\n\n(silence)\n"), "");
    }

    #[test]
    fn the_model_list_points_at_real_files() {
        for m in WHISPER_MODELS {
            assert!(m.file.starts_with("ggml-"));
            assert!(m.file.ends_with(".bin"));
            assert!(model_url(m.file).contains("whisper.cpp"));
        }
    }

    #[test]
    fn a_missing_model_names_where_it_looked() {
        let v = VoiceConfig {
            enabled: true,
            model: "/nonexistent/ggml-base.en.bin".into(),
            ..Default::default()
        };
        let e = find_model(&v).unwrap_err();
        assert!(e.to_string().contains("not there"), "got {e}");
    }

    #[test]
    fn the_recording_path_is_per_process_so_two_runs_cannot_collide() {
        assert!(
            scratch_wav()
                .to_string_lossy()
                .contains(&std::process::id().to_string())
        );
    }
}
