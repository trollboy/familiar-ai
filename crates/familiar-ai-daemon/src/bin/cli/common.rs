use familiar_ai_core::{AppPaths, Config};
use familiar_ai_storage::Database;

pub fn database() -> Result<Database, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    Ok(db)
}

pub fn effective_repository_config(
    paths: &AppPaths,
    repository: &std::path::Path,
) -> Result<Config, String> {
    familiar_ai_daemon::config_cli::effective_config_for_repository(
        &familiar_ai_daemon::config_cli::ConfigContext {
            config_path: paths.config_dir.join("config.toml"),
            data_dir: paths.data_dir.clone(),
        },
        repository,
    )
}
