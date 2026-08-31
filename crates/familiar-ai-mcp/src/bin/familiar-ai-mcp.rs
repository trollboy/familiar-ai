use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use familiar_ai_core::config::{Config, LoggingConfig};
use familiar_ai_core::control_plane::{OwnershipClaim, CONTROL_PROTOCOL_VERSION};
use familiar_ai_core::{AppPaths, AppStatus};
use familiar_ai_daemon::local_transport::{ClientHello, LocalClient};
use familiar_ai_mcp::storage::UnavailableStorage;
use familiar_ai_mcp::tool::{ToolContext, ToolRegistry};
use familiar_ai_mcp::tools::control_plane;
use familiar_ai_mcp::{McpServer, StdioTransport, Storage};

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

async fn run() -> familiar_ai_core::Result<()> {
    // 1. AppPaths
    let paths = AppPaths::resolve()?;
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
    let _log_guard = familiar_ai_logging::init_logging(&logging, None)?;

    tracing::info!("familiar-ai-mcp starting");

    // 4. Connect with a host-created capability reference. Neither the raw
    // credential nor the reference is emitted to model-visible output.
    let reference_path=std::env::var_os("FAMILIAR_AI_MCP_SESSION_FILE").map(std::path::PathBuf::from).ok_or_else(||familiar_ai_core::FamiliarError::Mcp("capability session file is required; launch MCP through the daemon-owned worker adapter".into()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = std::fs::metadata(&reference_path)?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(familiar_ai_core::FamiliarError::Mcp(
                "capability session file must be owned by the current user and mode 0600".into(),
            ));
        }
    }
    let credential = std::fs::read_to_string(&reference_path)?;
    let claim: OwnershipClaim = serde_json::from_str(&std::fs::read_to_string(
        paths.runtime_dir.join("control-plane.claim"),
    )?)
    .map_err(|e| {
        familiar_ai_core::FamiliarError::Mcp(format!("invalid control-plane claim: {e}"))
    })?;
    let client = LocalClient::connect(
        std::path::Path::new(&claim.socket_path),
        ClientHello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: format!("mcp-{}", std::process::id()),
            session_reference: Some(credential.trim().into()),
            owner_nonce: Some(claim.owner_nonce),
        },
    )
    .await?;
    let client = Arc::new(tokio::sync::Mutex::new(client));

    // 5. Build status snapshot
    let status = Arc::new(Mutex::new(AppStatus::new()));
    {
        let mut s = status.lock().unwrap();
        s.mcp_enabled = true;
    }

    // Agent-facing MCP never receives or initializes provider credentials.
    let router = None;

    // 7. Build storage + context + registry
    let storage: Arc<dyn Storage> = Arc::new(UnavailableStorage);
    let context = Arc::new(ToolContext {
        storage,
        status,
        config: Arc::new(config),
        router,
    });

    let mut registry = ToolRegistry::new();
    control_plane::register(&mut registry, client);
    let registry = Arc::new(registry);

    tracing::info!(tool_count = registry.len(), "MCP server ready");

    // 7. Build transport + server, run
    let transport = Box::new(StdioTransport::new());
    let server = McpServer::new(transport, registry, context);
    server.run().await?;

    tracing::info!("familiar-ai-mcp exiting");
    Ok(())
}
