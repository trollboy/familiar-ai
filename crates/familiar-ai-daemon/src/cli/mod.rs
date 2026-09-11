//! Per-domain CLI subcommand implementations. `bin/familiar-ai.rs` keeps
//! argument parsing and dispatch; the enums and handlers for each domain
//! live in their own module here so pending work can declare one module
//! file instead of the shared binary source.

pub mod billing;
pub mod config;
pub mod daemon_args;
pub mod worker;
