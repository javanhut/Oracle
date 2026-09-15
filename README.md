# Oracle

A local troubleshooting companion. It reads your system, tells you what looks
wrong, and suggests what to try. It runs when you run it and at no other time.

```sh
raven-oracle                                # the desktop app, also in the launcher
oracle tui                                  # the full-screen terminal interface
oracle doctor                               # the same findings as plain text
oracle ask "why can't I install anything"
rvn install foo 2>&1 | oracle explain
```

## It is not part of Raven Linux

Oracle is a separate program that happens to know about Raven Linux. Nothing in
the base system requires it, references it, or knows it exists.

- It is in no install profile. A fresh Raven install does not have it.
- It adds no service to `/etc/raven/init.d` and no script to
  `/etc/raven/session.d`.
- It has no autostart entry, no tray icon and no background service. The
  desktop app sits in the launcher like any other Raven app and runs while its
  window is open. The one exception is an answer still coming from the model:
  closing the window then hides it until the answer is ready, a notification
  says so, and the app ends once the answer has been opened or dismissed.
- Installing it puts two binaries, a launcher entry, its metainfo and an icon
  under `/usr/local`. Nothing in `/etc` is touched.
- Uninstalling removes all of that. `oracle forget` (or *Forget Everything* in
  the app's Settings) deletes the config and anything downloaded first, if you
  want a clean exit.

This restraint is the design rather than a gap in it. A troubleshooting
assistant that starts at login, watches your machine, and volunteers advice is a
different and much worse product. It is the thing people switch off in the first
week. This one waits to be asked.

## What it does

**Checks the machine, without a model.** The rules find real problems with no
inference server, no downloaded weights and no network. That is the floor, not a
degraded mode.

```
warning: pacman cannot read the package database, though rvn can
  pacman failed: error: failed to initialize alpm library: (root: /, dbpath:
    /var/lib/pacman) database is incorrect version try running pacman-db-upgrade
  Ordinary installs will work, and anything that shells out to the broken tool
    will not. AUR builds run makepkg, which calls pacman, so they are the usual
    casualty.
  Migrate the database to the format the installed tools expect
    sudo pacman-db-upgrade
```

**Finds failures nothing else reports.** A `raven-init` service with
`critical = false` whose executable is missing fails at every boot and tells
nobody. Oracle compares every enabled service's declared `exec` against the
filesystem, which turns that class of bug from invisible into obvious. Firmware
the kernel asked for and did not get is the same shape of problem: the device
does not appear broken, it appears absent.

**Answers questions, if you have a local model.** `oracle ask` gathers only the
part of the system the question is about, hands it to a model on this machine,
and prints the answer. The findings come first regardless, because they are
certain and the model's paragraph is not.

**Takes dictation, if you set it up.** Optional, off by default, covered below.

## The desktop app

`raven-oracle` is Oracle as a Raven app: the same GTK4 and libadwaita stack,
the same Raven Glass look, and the same sidebar-and-pages shape as Settings,
Store and Power. It follows the desktop's theme mode, accent and transparency
from `~/.config/raven/desktop.toml`.

| Page | |
|---|---|
| Overview | The verdict, counts by severity, the machine, and how Oracle behaves |
| Findings | Worst first, each with what Oracle saw and what to try. Copy a command; ask the model to explain one |
| Ask | A question about this machine, answered by the local model as it streams |
| Explain an Error | Paste what a failed command printed and get told what it means |
| What a Model Sees | The exact text a model would be given, before anything is sent |
| Settings | The model, what Oracle may read, privacy, and *Forget Everything* |

It holds to the same rules as the command line. It checks the machine when the
window opens and when you press *Check again* (or Ctrl+R), never on a timer. It
never runs a command; it copies one to the clipboard and you decide. Closing the
window ends it. Settings writes the same `oracle.toml` that `oracle setup` does,
so the two never disagree, and a file that does not parse stops the app with an
explanation rather than being quietly replaced by the defaults.

The app is a Cargo feature, on by default, and can be compiled out:

```sh
cargo build --no-default-features --features tui    # oracle and the TUI, no GTK
```

## The terminal interface

`oracle tui` opens a full-screen view of the same findings: a list on the left,
the selected finding's evidence and suggested steps on the right.

```
 oracle · read-only · 1 critical 2 warnings 3 notes
┏ findings ━━━━━━━━━━━━━━━━━━━━━━┓┌ detail ──────────────────────────────────┐
┃▍critical  / is 99% full        ┃│ / is 99% full                            │
┃ warning   pacman cannot read…  ┃│                                          │
┃ note      dbus is logging err… ┃│ What I saw                               │
┃                                ┃│   · 99G used of 100G, leaving 1G free    │
┃                                ┃│                                          │
┃                                ┃│ What I would try                         │
┃                                ┃│   · see where the space went             │
┃                                ┃│       du -xh --max-depth=2 /             │
┃                                ┃│                                          │
┃                                ┃│ Oracle does not run these. Press y to    │
┃                                ┃│ copy the first one.                      │
┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛└──────────────────────────────────────────┘
  ↑↓ move · tab pane · y copy command · e explain · a ask · f filter · r recheck
```

| Key | |
|---|---|
| `↑` `↓` `j` `k` | Move the selection. `tab` swaps panes so you can scroll the detail |
| `f` | Cycle the filter: everything, warnings and worse, critical only |
| `r` | Check the machine again |
| `a` | Ask a question. The answer streams in as the model produces it |
| `e` | Have the model explain the selected finding |
| `y` | Copy that finding's command to the clipboard |
| `?` | Keys |
| `esc` | Back, or stop an answer mid-stream |
| `q` | Quit |

It holds to the same rules as everything else. It opens only when you type
`oracle tui`, never on a bare `oracle`. It never runs a command; `y` copies one
and you decide. It does not poll, so checks happen on start and when you press
`r` and at no other time. And it gives the terminal back exactly as it found it,
including when it panics, because a crash that leaves someone in raw mode with
no cursor has done more harm than the bug did.

The interface is a Cargo feature, on by default:

```sh
cargo build --no-default-features     # checks only, neither interface
```

Oracle's other three dependencies are small and pure Rust. ratatui brings a
hundred-odd crates with it, which is a fair price for a terminal UI that handles
resizing and Unicode correctly, and the wrong price for a minimal image that
only ever runs `oracle doctor`. A build without it says so and points at the
command that does the same job.

## What it will not do

| | |
|---|---|
| Run in the background | There is no daemon, no timer, and no autostart. There is no code here that could add one. |
| Change your system | Every probe is a read. It prints commands; you decide. |
| Send anything anywhere | A model endpoint off this machine is refused unless you deliberately permit it, http or https alike. |
| Install things for you | It will tell you what is missing, once, and stop. |
| Nag | No first-run wizard, no prompts at the end of other commands, no telling you twice. |

`oracle context` prints the exact text a model would be given, without sending
it. The privacy claims are checkable rather than believable.

## Install

```sh
imlazy install          # or: sudo make install
imlazy install-cli      # or: sudo make install-cli -- the command alone, no GTK
```

`install` puts `oracle` and `raven-oracle` in `/usr/local/bin`, and the launcher
entry, metainfo and icon under `/usr/local/share`, the same way Raven Store and
Raven Settings install. Nothing is enabled and nothing in `/etc` is touched.

To leave:

```sh
oracle forget           # deletes the config and any downloaded models
imlazy uninstall        # removes the binaries, launcher entry and icon
```

## Commands

| | |
|---|---|
| `raven-oracle` | The desktop app. `--page <id>` opens on a page |
| `oracle tui` | The full-screen terminal interface |
| `oracle doctor` | Check the machine and report |
| `oracle ask "…"` | Ask about this machine |
| `oracle explain` | Explain an error you paste or pipe in |
| `oracle context` | Show exactly what would be sent to a model |
| `oracle status` | What is configured and what is missing |
| `oracle setup` | Configure it. Optional; the defaults work |
| `oracle voice …` | Dictation: `setup`, `status`, `models`, `fetch`, `test` |
| `oracle listen` | Dictate a question, confirm it, then ask it |
| `oracle dictate` | Transcribe speech to stdout and nothing else |
| `oracle forget` | Delete the config and downloaded models |

A bare question works too: `oracle why is the store so slow`.

`--area` narrows what gets read to any of `services`, `storage`, `network`,
`packages`, `logs`, `hardware`. `--json` gives the whole report as data.
`--strict` makes the exit code follow the worst finding, for a script that wants
one; by default `doctor` always exits 0, because a diagnostic that fails a build
over a note is a diagnostic people stop running.

Two categories of check are off unless asked for. `--online` permits checks that
emit network traffic, and `--slow` permits ones that are slow or that spawn the
package manager. A passive read of the routing table answers most questions, and
a tool that quietly sends packets is not a quiet tool.

## The local model

Oracle works without one. With one, it can answer open questions.

It speaks to two kinds of server, both on this machine:

- **Ollama** at `http://127.0.0.1:11434`, the default.
- **llama.cpp** (`llama-server`) at `http://127.0.0.1:8080`, which also covers
  anything else offering the OpenAI chat-completions shape.

Oracle never downloads a language model and never installs a server. If you want
one:

```sh
rvn install ollama
ollama serve &
ollama pull qwen2.5:3b-instruct
```

On a laptop with integrated graphics, a 3B instruct model is the comfortable
size. Oracle picks a small instruction-tuned model automatically when you have
not named one, and says so rather than quietly substituting a different one if
the model you configured has been removed.

The system prompt's main job is keeping a small model honest. Asked an open
question about Linux, one will confidently invent a systemd unit, a config file
that does not exist, and a package from a different distribution. Oracle holds it
to the evidence it was given and makes "I don't know from what I can see here" an
acceptable answer, because on a troubleshooting tool a confident wrong answer
costs more than no answer.

## Dictation

Entirely optional and off until you turn it on.

```sh
rvn install whisper.cpp
oracle voice fetch base.en
oracle voice setup
```

**whisper.cpp** is the default because it is the one that fits: a single binary
with no runtime, GGML weights from 60MB up, and usable accuracy on four cores
with no GPU. The alternatives each fail one of those tests. faster-whisper wants
a Python and CTranslate2 stack, Vosk is meaningfully less accurate, and the good
NVIDIA models want an NVIDIA card. Set `[voice] command` in the config to use
something else; anything that takes a WAV path and prints text will do.

`oracle voice models` lists what is worth fetching, with sizes. On a laptop CPU,
`base.en` is roughly real time and `small.en` is about three times slower for a
noticeable accuracy gain.

How it behaves, which is not configurable:

- Recording happens only while `oracle listen`, `oracle dictate` or
  `oracle voice test` is running, and the terminal says so while it does.
- There is no hotword and no background listener. That is not a setting.
- The audio file is deleted as soon as it has been transcribed, on every path
  including failure.
- The transcript is shown to you before it is used for anything.

`oracle dictate` prints the transcript to stdout and nothing else, so it
composes: `oracle dictate | wl-copy`.

## Privacy

Redaction runs over everything Oracle gathers before it reaches a model or a
report: keys, passphrases, tokens, email addresses, MAC addresses, your username,
and routable IP addresses, IPv4 and IPv6 alike. Private and loopback addresses
stay, because they are what makes a network diagnosis possible and they say
nothing about where you are. The one exception is an IPv6 address whose second
half was built from the MAC (EUI-64): that half is masked on any address,
link-local included, because it is the MAC.

The model endpoint must be on this machine unless you say otherwise.
`Config::endpoint` refuses a non-loopback address until `allow_remote_endpoint`
is deliberately set (in the app, *Allow a model on another machine*). Local is
enforced by the code rather than promised by this file.

`https://` endpoints work, for a server behind a TLS proxy or on another
machine. The connection goes through rustls and is verified against the
system's trusted certificates, and there is no option to skip verification.
HTTPS changes how the bytes travel, not where Oracle may send them: an
`https://` address off this machine is refused exactly as an `http://` one is.

Nothing is logged. History is off by default, so Oracle does not accumulate a
record of your problems unless you ask it to.

## Configuration

There is no config file until you run `oracle setup`. Every setting has a working
default, so a missing file is not an error and never produces a prompt to create
one.

`~/.config/raven/oracle.toml`, if it exists:

```toml
[model]
backend = "ollama"                 # ollama | llamacpp | none
endpoint = "http://127.0.0.1:11434"
name = ""                          # empty: pick a small instruct model

[context]                          # each switch stops a probe from running
services = true
storage = true
network = true
packages = true
logs = true
hardware = true

[privacy]
redact = true
allow_remote_endpoint = false
keep_history = false

[voice]
enabled = false
whisper_bin = ""                   # empty: look on PATH
model = ""                         # empty: look in the usual places
```

A switch under `[context]` set to `false` means the probe does not run at all.
Its data cannot reach a model by any route, and `doctor` says which areas it was
told not to look at, so an empty report is never mistaken for a clean bill of
health it did not earn.

A file that exists but does not parse is a hard error rather than a silent
fallback, for the reason `ravend` gives about `login.toml`: falling back would
quietly ignore a policy somebody wrote down and believed.

## Building

```sh
imlazy build            # or: make
imlazy gui              # build and open the desktop app
imlazy -w gui           # ...and relaunch it whenever the source changes
imlazy snapshot         # render every page of the app to target/snapshots
imlazy build-cli        # the command line and TUI without GTK
imlazy build-minimal    # the checks alone, neither interface
imlazy test
imlazy check            # fmt, clippy and tests, both feature sets
imlazy smoke            # drive the terminal interface in a real pty
```

The checks, rules, model client and prompts are a library (`src/lib.rs`) that
both front ends use, so a finding reads the same wherever it is reported.
The core has five dependencies: `serde`, `serde_json` and `toml` for the
checks, and `rustls` with `rustls-native-certs` for https to a model server. The terminal interface adds `ratatui`, and the desktop app adds
the gtk-rs crates at the same versions as the other Raven apps; either can be
compiled out. The desktop app's state and decisions live in a GTK-free module
with its own tests, the same split the terminal interface makes.
The HTTP client is a few hundred lines here rather than a crate, which keeps the
tree small and follows what the rest of the Raven layer already does. HTTPS is
rustls with the ring backend and the system's certificate store, so there is no
OpenSSL in the binary.

The interface is tested without a terminal and then with one. Its state machine
is a plain type with no drawing in it, frames are rendered to ratatui's test
backend and asserted on as text, and a script drives the real binary in a pty to
confirm it paints, streams an answer, and hands the terminal back.

## Licence

MIT.
