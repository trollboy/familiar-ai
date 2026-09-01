//! `familiar-ai waive` — attach a durable human waiver to one open blocking
//! review finding. Completion-evidence requires a durable human waiver for
//! every open blocking finding of a terminal review; this is the operator
//! surface for creating one (FAM-FRICTION-008). Waivers are stored with the
//! finding's claim substance so they survive reviewer id rotation between
//! attempts (FAM-BUG-044).

use familiar_ai_core::AppPaths;
use familiar_ai_storage::{Database, ReviewRepository};

use super::shared::effective_repository_config;

pub fn waive(
    cycle_id: String,
    finding_id: String,
    actor: String,
    reason: String,
) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &current)?;
    let db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    let waiver = ReviewRepository::new(db.conn())
        .waive_finding(&cycle_id, &finding_id, &actor, &reason)
        .map_err(|e| e.to_string())?;
    println!(
        "waived {} on cycle {} by {} at {}",
        waiver.finding_id, waiver.cycle_id, waiver.actor, waiver.created_at
    );
    Ok(())
}
