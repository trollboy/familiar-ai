//! Per-domain subcommand implementations for the `familiar-ai` binary. Each
//! module owns both the clap subcommand definitions for its domain and the
//! function bodies that carry them out; `src/bin/familiar-ai.rs` stays
//! dispatch-only (argument parsing plus a match into these modules).

pub mod backlog;
pub mod billing;
pub mod common;
pub mod compress;
pub mod config;
pub mod control;
pub mod deliver;
pub mod drive;
pub mod history;
pub mod next;
pub mod onboard;
pub mod plan;
pub mod preflight;
pub mod report;
pub mod resume;
pub mod run;
pub mod scope_decisions;
pub mod stewardship;
pub mod usage;
pub mod worker;
