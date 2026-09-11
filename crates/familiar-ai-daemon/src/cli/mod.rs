pub mod backlog;
pub mod billing;
pub mod control;
pub mod delivery;
pub mod driver;
pub mod onboard;
pub mod plan;
pub mod stewardship;
pub mod worker;

use familiar_ai_core::{AppPaths, Config};
use familiar_ai_storage::Database;

/// Open the daemon's default database at its configured path, applying
/// pending migrations. Shared by every domain that reads or writes durable
/// state outside a specific repository's backlog context.
pub(crate) fn database() -> Result<Database, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    Ok(db)
}

/// Resolve the approval-aware three-layer configuration for one repository.
/// Shared by every domain that must honor repository-scoped configuration
/// (backlog discovery, planning, preflight, worker installation).
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
