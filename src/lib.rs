//! Oracle's core: the probes, the rules, the model client and the prompts.
//!
//! Two front ends sit on top of this. `oracle` is the command line and the
//! terminal interface; `raven-oracle` is the desktop app. Neither has any
//! diagnostic logic of its own, so a finding reads the same whichever one
//! reported it.

pub mod config;
pub mod diagnose;
pub mod http;
pub mod model;
pub mod probe;
pub mod prompt;
pub mod redact;
pub mod report;
pub mod setup;
pub mod stt;
pub mod sys;
pub mod ui;
