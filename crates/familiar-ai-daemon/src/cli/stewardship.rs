//! `familiar-ai stewardship` — query durable execution-era state
//! (backlog, sessions, attempts, worktrees, review findings, budgets,
//! delivery, recovery events, and pending human gates) for the current
//! repository. Read-only; prints one JSON object per invocation.

use clap::Subcommand;
use familiar_ai_core::{BacklogDiscovery, FilesystemBacklogDiscovery};

use super::shared::database;

#[derive(Debug, Subcommand)]
pub enum StewardshipCommand {
    /// List the backlog graph.
    Backlog {
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List driver sessions, most recent first.
    Sessions {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List one session's attempts, including worktree/branch identity.
    Attempts {
        session_id: String,
        #[arg(long)]
        cursor: Option<i64>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List execution checkpoints (worktree/branch identity, recovery phase).
    Checkpoints {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List audited backlog recovery events.
    Recovery {
        #[arg(long)]
        cursor: Option<i64>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List delivery authority decisions.
    Delivery {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show one session's warrant, cost, and delivery-warrant consumption.
    Budget { session_id: String },
    /// Show review disposition and blocking scope findings for one session.
    Review { session_id: String },
    /// List stopped attempts and blocked checkpoints awaiting a human decision.
    Gates {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show current-effective cost reconciliation (PRD-053) for this
    /// repository's durable project over a UTC range.
    Reconciliation { start: String, end: String },
}

/// Read-only: shares its query implementation with the dashboard's
/// `/stewardship/*` endpoints.
pub fn stewardship_command(command: StewardshipCommand) -> Result<(), String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let db = database()?;
    let value = match command {
        StewardshipCommand::Backlog {
            status,
            cursor,
            limit,
        } => crate::stewardship::list_backlog(
            &db,
            &repository,
            status.as_deref(),
            cursor.as_deref(),
            limit,
        ),
        StewardshipCommand::Sessions { cursor, limit } => {
            crate::stewardship::list_sessions(&db, &repository, cursor.as_deref(), limit)
        }
        StewardshipCommand::Attempts {
            session_id,
            cursor,
            limit,
        } => crate::stewardship::list_attempts(&db, &repository, &session_id, cursor, limit),
        StewardshipCommand::Checkpoints { cursor, limit } => {
            crate::stewardship::list_checkpoints(&db, &repository, cursor.as_deref(), limit)
        }
        StewardshipCommand::Recovery { cursor, limit } => {
            crate::stewardship::list_recovery_events(&db, &repository, cursor, limit)
        }
        StewardshipCommand::Delivery { cursor, limit } => {
            crate::stewardship::list_delivery_decisions(&db, &repository, cursor.as_deref(), limit)
        }
        StewardshipCommand::Budget { session_id } => {
            crate::stewardship::get_budget(&db, &repository, &session_id)
        }
        StewardshipCommand::Review { session_id } => {
            crate::stewardship::list_review_findings(&db, &repository, &session_id)
        }
        StewardshipCommand::Gates { limit } => {
            crate::stewardship::list_pending_human_gates(&db, &repository, limit)
        }
        StewardshipCommand::Reconciliation { start, end } => {
            crate::stewardship::get_reconciliation(&db, &repository, &start, &end)
        }
    }
    .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    );
    Ok(())
}
