//! `familiar-ai scope-decisions` — list or decide one hash-bound pending
//! scope finding.

use std::io::{self, IsTerminal, Write};

use familiar_ai_core::{AppPaths, BacklogDiscovery, Config, FilesystemBacklogDiscovery};
use familiar_ai_storage::Database;

use super::shared::effective_repository_config;

pub fn scope_decisions(
    finding_hash: Option<String>,
    candidate_hash: Option<String>,
    approve: bool,
    reject: bool,
    actor: Option<String>,
    reason: Option<String>,
) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &current)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|e| e.to_string())?;
    let mut db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    let repo = familiar_ai_storage::OrchestrationRepository::new(db.conn());
    let pending = repo
        .pending_scope_decisions(&repository.key)
        .map_err(|e| e.to_string())?;
    if finding_hash.is_none() {
        for item in &pending {
            println!(
                "{}",
                serde_json::to_string(item).map_err(|e| e.to_string())?
            );
        }
        if !pending.is_empty() && io::stdin().is_terminal() && io::stderr().is_terminal() {
            eprint!("Decide finding hash (blank to preserve): ");
            io::stderr().flush().map_err(|e| e.to_string())?;
            let mut hash = String::new();
            io::stdin()
                .read_line(&mut hash)
                .map_err(|e| e.to_string())?;
            let hash = hash.trim();
            if hash.is_empty() {
                return Ok(());
            }
            let item = pending
                .iter()
                .find(|p| p.finding_hash == hash)
                .ok_or_else(|| "pending finding hash not found".to_string())?;
            eprint!("Approve or reject [a/r]: ");
            io::stderr().flush().map_err(|e| e.to_string())?;
            let mut choice = String::new();
            io::stdin()
                .read_line(&mut choice)
                .map_err(|e| e.to_string())?;
            eprint!("Actor (human:<identity>): ");
            io::stderr().flush().map_err(|e| e.to_string())?;
            let mut who = String::new();
            io::stdin().read_line(&mut who).map_err(|e| e.to_string())?;
            let checkpoint = repo
                .decide_scope(
                    &repository.key,
                    hash,
                    &item.candidate_hash,
                    choice.trim().eq_ignore_ascii_case("a"),
                    who.trim(),
                    "interactive scope decision",
                )
                .map_err(|e| e.to_string())?;
            continue_scope_decision(&mut db, &repository, &config, &checkpoint)?;
        }
        return Ok(());
    }
    if approve == reject {
        return Err("supply exactly one of --approve or --reject".into());
    }
    let actor = actor.ok_or_else(|| "--actor is required for a decision".to_string())?;
    if !actor.starts_with("human:") {
        return Err("--actor must be human:<identity>".into());
    }
    let checkpoint = repo
        .decide_scope(
            &repository.key,
            &finding_hash.unwrap(),
            &candidate_hash.ok_or_else(|| "--candidate-hash is required".to_string())?,
            approve,
            &actor,
            &reason
                .filter(|r| !r.trim().is_empty())
                .ok_or_else(|| "--reason is required".to_string())?,
        )
        .map_err(|e| e.to_string())?;
    continue_scope_decision(&mut db, &repository, &config, &checkpoint)?;
    Ok(())
}

fn continue_scope_decision(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    config: &Config,
    checkpoint_id: &str,
) -> Result<(), String> {
    let checkpoint = familiar_ai_storage::CheckpointRepository::new(db.conn())
        .all(&repository.key)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|checkpoint| checkpoint.checkpoint_id == checkpoint_id)
        .ok_or_else(|| format!("checkpoint {checkpoint_id} disappeared"))?;
    if checkpoint.phase != "reviewed" {
        return Ok(());
    }
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|error| error.to_string())?;
    let target = FilesystemBacklogDiscovery
        .discover_with_layout(repository, &repository_config.layout())
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|target| target.id.to_string() == checkpoint.prd_id)
        .ok_or_else(|| format!("{} is no longer discoverable", checkpoint.prd_id))?;
    crate::drive::continue_scope_approved_candidate(db, repository, &target, config)
}
