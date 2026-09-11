//! Library-side implementation of the `familiar-ai` CLI binary's
//! subcommands. `crates/familiar-ai-daemon/src/bin/familiar-ai.rs` owns the
//! top-level `Cli`/`Command` clap types and `fn main()`'s dispatch; the
//! actual per-subcommand argument shapes and handlers live here, one module
//! per domain, following the pattern already established by the sibling
//! library modules `config_cli` and `compress_cli`.

pub mod backlog;
pub mod billing;
pub mod compress;
pub mod config;
pub mod control;
pub mod deliver;
pub mod drive;
pub mod onboard;
pub mod plan;
pub mod preflight;
pub mod report;
pub mod run;
pub mod scope;
pub mod stewardship;
pub mod usage;
pub mod worker;

use familiar_ai_core::{AppPaths, Config};
use familiar_ai_storage::Database;

pub(crate) fn database() -> Result<Database, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    Ok(db)
}

pub(crate) fn effective_repository_config(
    paths: &AppPaths,
    repository: &std::path::Path,
) -> Result<Config, String> {
    crate::config_cli::effective_config_for_repository(
        &crate::config_cli::ConfigContext {
            config_path: paths.config_dir.join("config.toml"),
            data_dir: paths.data_dir.clone(),
        },
        repository,
    )
}
