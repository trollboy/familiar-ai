//! Implementation bodies for every `familiar-ai` CLI subcommand.
//!
//! The clap argument schema (the `Cli`/`Command` derive types and, where
//! shown below, the domain's nested `#[derive(Subcommand)]` enum) lives in
//! `bin/familiar-ai.rs`; the enums that gate on a shared runtime/database
//! setup live here instead, next to the code that matches on them, because a
//! library module cannot reference a type defined only in a binary crate.
//! Every function here is a verbatim move of what used to live inline in
//! `bin/familiar-ai.rs`.
//!
//! PRD-090: the modules below are unchanged by the CLI surface design pass.
//! `bin/familiar-ai.rs` now presents them under a small set of declared
//! administrative namespaces (`config`, `accounting`, `stewardship`, `plan`,
//! `ops`) instead of at the top level, alongside the daily verbs (`next`,
//! `run`, `drive`, `resume`, `report`, `approve`, `deliver`) and the
//! no-argument front door. Every previous top-level invocation keeps working
//! as a hidden alias -- see [`shared::RELOCATED_COMMAND_ALIASES`].

pub mod accounting;
pub mod backlog;
pub mod batch_review;
pub mod billing;
pub mod control;
pub mod deliver;
pub mod desktop;
pub mod drive;
pub mod gate;
pub mod history;
pub mod metrics;
pub mod model_residency;
pub mod next;
pub mod onboard;
pub mod operator;
pub mod plan;
pub mod preflight;
pub mod report;
pub mod resume;
pub mod run;
pub mod scope_decisions;
pub mod shared;
pub mod stewardship;
pub mod usage;
pub mod waive;
pub mod worker;
pub mod workers;
