use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use familiar_core::config::{Config, LoggingConfig};
use familiar_core::{AppPaths, AppStatus};
use familiar_mcp::storage::SqliteStorage;
use familiar_mcp::tool::{ToolContext, ToolRegistry};
use familiar_mcp::tools::register_default_tools;
use familiar_mcp::{McpServer, StdioTransport, Storage};
use familiar_storage::Database;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> familiar_core::Result<()> {
    // 1. AppPaths
    let paths = AppPaths::new();
    paths.ensure_dirs()?;

    // 2. Load config
    let config_path = paths.config_dir.join("config.toml");
    let config = Config::load(Some(&config_path))?;

    // 3. Init logging — STDERR ONLY. stdout is reserved for the MCP transport.
    //    Force file output to None so we don't fight the daemon's log file.
    let logging = LoggingConfig {
        file: None,
        ..config.logging.clone()
    };
    let _log_guard = familiar_logging::init_logging(&logging, None)?;

    tracing::info!("familiar-mcp starting");

    // 4. Open database (read/write — remember_result needs writes)
    let db_path = config.database.resolve_path(&paths.data_dir);
    tracing::info!(db_path = %db_path.display(), "opening database");
    let db = Arc::new(Mutex::new(Database::open(&db_path)?));
    {
        let lock = db.lock().unwrap();
        lock.run_migrations()?;
    }

    // 5. Build status snapshot
    let status = Arc::new(Mutex::new(AppStatus::new()));
    {
        let mut s = status.lock().unwrap();
        s.mcp_enabled = true;
    }

    // 6. Build inference router if configured
    // NOTE: Each process owns its own InferenceRouter from on-disk config.
    // See InferenceRouter docs for rationale.
    let router = {
        let r = Arc::new(familiar_llm::InferenceRouter::new(&config.inference));
        if config.inference.text.mode != familiar_core::config::InferenceMode::Disabled {
            if let Err(e) = r.enable().await {
                tracing::warn!(error = %e, "MCP: failed to enable inference router");
            }
        }
        Some(r)
    };

    // 7. Build storage + context + registry
    let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(db));
    let context = Arc::new(ToolContext {
        storage,
        status,
        config: Arc::new(config),
        router,
    });

    let mut registry = ToolRegistry::new();
    register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    tracing::info!(tool_count = registry.len(), "MCP server ready");

    // 7. Build transport + server, run
    let transport = Box::new(StdioTransport::new());
    let server = McpServer::new(transport, registry, context);
    server.run().await?;

    tracing::info!("familiar-mcp exiting");
    Ok(())
}
