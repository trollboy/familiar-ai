//! Implementation bodies for every `familiar-ai` CLI subcommand.
//!
//! The clap argument schema (the `Cli`/`Command` derive types and, where
//! shown below, the domain's nested `#[derive(Subcommand)]` enum) lives in
//! `bin/familiar-ai.rs`; the enums that gate on a shared runtime/database
//! setup live here instead, next to the code that matches on them, because a
//! library module cannot reference a type defined only in a binary crate.
//! Every function here is a verbatim move of what used to live inline in
//! `bin/familiar-ai.rs`.

pub mod accounting;
pub mod backlog;
pub mod billing;
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
pub mod shared;
pub mod stewardship;
pub mod usage;
pub mod worker;
