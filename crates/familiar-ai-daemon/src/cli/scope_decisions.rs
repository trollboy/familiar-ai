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

    if finding_hash.is_none() {
        let pending = load_pending(&mut db, &repository.key)?;
        // A pipe gets the machine-readable enumeration, unconditionally.
        // This is the only programmatic listing of pending scope decisions,
        // so prose must never replace it — scripts parse these lines.
        if !(io::stdin().is_terminal() && io::stderr().is_terminal()) {
            for item in &pending {
                println!(
                    "{}",
                    serde_json::to_string(item).map_err(|e| e.to_string())?
                );
            }
            return Ok(());
        }
        if pending.is_empty() {
            println!("No pending scope decisions.");
            return Ok(());
        }
        return decide_interactively(&mut db, &repository, &config);
    }

    if approve == reject {
        return Err("supply exactly one of --approve or --reject".into());
    }
    let actor = actor.ok_or_else(|| "--actor is required for a decision".to_string())?;
    if !actor.starts_with("human:") {
        return Err("--actor must be human:<identity>".into());
    }
    let finding = finding_hash.unwrap();
    let candidate = candidate_hash.ok_or_else(|| "--candidate-hash is required".to_string())?;
    let reason = reason
        .filter(|r| !r.trim().is_empty())
        .ok_or_else(|| "--reason is required".to_string())?;
    let checkpoint = decide_one(
        &mut db,
        &repository.key,
        &finding,
        &candidate,
        approve,
        &actor,
        &reason,
    )?;
    continue_scope_decision(&mut db, &repository, &config, &checkpoint)?;
    Ok(())
}

/// Pending rows, read through a scoped borrow so the caller keeps `&mut db`.
fn load_pending(
    db: &mut Database,
    repository_key: &str,
) -> Result<Vec<familiar_ai_storage::ScopeDecision>, String> {
    let repo = familiar_ai_storage::OrchestrationRepository::new(db.conn());
    repo.pending_scope_decisions(repository_key)
        .map_err(|e| e.to_string())
}

fn decide_one(
    db: &mut Database,
    repository_key: &str,
    finding_hash: &str,
    candidate_hash: &str,
    approve: bool,
    actor: &str,
    reason: &str,
) -> Result<String, String> {
    let repo = familiar_ai_storage::OrchestrationRepository::new(db.conn());
    repo.decide_scope(
        repository_key,
        finding_hash,
        candidate_hash,
        approve,
        actor,
        reason,
    )
    .map_err(|e| e.to_string())
}

/// One human-readable line: what changed, in which PRD, and why it stopped.
fn describe(item: &familiar_ai_storage::ScopeDecision) -> String {
    let finding: serde_json::Value =
        serde_json::from_str(&item.finding_json).unwrap_or(serde_json::Value::Null);
    let field = |name: &str| {
        finding
            .get(name)
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string()
    };
    format!(
        "{:9} {:12} {}\n              {}",
        item.prd_id,
        field("change_kind"),
        field("path"),
        field("rule_detail")
    )
}

/// The flag form for one pending row, so an operator is never left without a
/// command they can run (or paste into a ticket).
fn command_for(item: &familiar_ai_storage::ScopeDecision, verb: &str) -> String {
    format!(
        "familiar-ai scope-decisions --finding-hash {} --candidate-hash {} --{verb} \\\n    --actor human:<you> --reason \"<why>\"",
        item.finding_hash, item.candidate_hash
    )
}

fn prompt(label: &str) -> Result<String, String> {
    eprint!("{label}");
    io::stderr().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

/// Numbered picker over every pending row. Loops until the operator quits or
/// nothing is left, because a paused attempt routinely carries more than one
/// finding and deciding only the first leaves it blocked for a reason the
/// surface just said was resolved.
fn decide_interactively(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    config: &Config,
) -> Result<(), String> {
    loop {
        let pending = load_pending(db, &repository.key)?;
        if pending.is_empty() {
            println!("\nAll scope decisions resolved.");
            return Ok(());
        }
        println!("\n{} pending scope decision(s):\n", pending.len());
        for (index, item) in pending.iter().enumerate() {
            println!("  {}. {}", index + 1, describe(item));
        }
        let selection = prompt(&format!("\nDecide [1-{} | all | q]: ", pending.len()))?;
        if selection.is_empty() || selection.eq_ignore_ascii_case("q") {
            println!("\nLeft undecided. To decide without this picker:\n");
            for item in &pending {
                println!("{}\n", command_for(item, "approve"));
            }
            return Ok(());
        }
        let chosen: Vec<_> = if selection.eq_ignore_ascii_case("all") {
            pending.iter().collect()
        } else {
            match selection.parse::<usize>() {
                Ok(n) if n >= 1 && n <= pending.len() => vec![&pending[n - 1]],
                _ => {
                    eprintln!("Not a choice: {selection:?}");
                    continue;
                }
            }
        };
        let verdict = prompt("Approve or reject [a/r]: ")?;
        let approve = match verdict.to_ascii_lowercase().as_str() {
            "a" | "approve" => true,
            "r" | "reject" => false,
            _ => {
                eprintln!("Not a verdict: {verdict:?}");
                continue;
            }
        };
        let actor = prompt("Actor (human:<identity>): ")?;
        if !actor.starts_with("human:") {
            eprintln!("Actor must start with 'human:' — nothing decided.");
            continue;
        }
        let reason = prompt("Reason: ")?;
        if reason.is_empty() {
            eprintln!("A reason is required — nothing decided.");
            continue;
        }
        // Decide every selected row before returning, so "all" means all.
        let targets: Vec<(String, String)> = chosen
            .iter()
            .map(|item| (item.finding_hash.clone(), item.candidate_hash.clone()))
            .collect();
        let fallbacks: Vec<String> = chosen
            .iter()
            .map(|item| command_for(item, if approve { "approve" } else { "reject" }))
            .collect();
        for (index, (finding, candidate)) in targets.iter().enumerate() {
            match decide_one(
                db,
                &repository.key,
                finding,
                candidate,
                approve,
                &actor,
                &reason,
            ) {
                Ok(checkpoint) => {
                    println!("recorded {finding}");
                    continue_scope_decision(db, repository, config, &checkpoint)?;
                }
                // Never a dead end: a failed decision still leaves the
                // operator holding the command that would have made it.
                Err(error) => {
                    eprintln!("could not decide {finding}: {error}");
                    eprintln!("run this instead:\n{}\n", fallbacks[index]);
                }
            }
        }
    }
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
