//! `familiar-ai operator` — repairs that used to live in `examples/`.
//!
//! These three tools existed as `cargo run --example` scripts, which meant an
//! operator repairing a stuck checkpoint needed the source tree, a toolchain,
//! and the knowledge that the scripts existed at all. PRD-084 promotes them to
//! shipped commands on the installed binary.
//!
//! Every one of them writes durable orchestration state or reads the same
//! scheduler the drive uses, so each demands an explicit human actor and a
//! reason, and each refuses while a driver owns the control-plane claim
//! (FAM-BUG-048) rather than racing it.

use std::path::Path;

use clap::Subcommand;

use familiar_ai_core::{AppPaths, BacklogDiscovery, FilesystemBacklogDiscovery};
use familiar_ai_storage::{CheckpointRepository, Database};

use super::shared::effective_repository_config;

#[derive(Debug, Subcommand)]
pub enum OperatorCommand {
    /// Rebind a checkpoint to its worktree's current candidate content after
    /// a surgical repair, recomputing the snapshot the way freeze does.
    Rebind {
        prd_id: String,
        /// Rebase the checkpoint onto a different base revision first.
        #[arg(long)]
        new_base: Option<String>,
        /// Mandatory explicit human authority in the form human:<identity>.
        #[arg(long)]
        actor: String,
        /// Mandatory non-empty audit reason.
        #[arg(long)]
        reason: String,
    },
    /// Transition a checkpoint's phase through the audited transition API.
    SetPhase {
        checkpoint_id: String,
        phase: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
    /// Ask the scheduler its achievable width for the remaining backlog, or
    /// for an explicitly named subset.
    Width {
        prds: Vec<String>,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
}

/// Authority is the same shape the backlog recovery paths demand: a named
/// human and a reason, both refused when empty rather than defaulted.
fn require_authority(actor: &str, reason: &str) -> Result<(), String> {
    if !actor.starts_with("human:") || actor.trim() == "human:" {
        return Err("--actor must be human:<identity>".into());
    }
    if reason.trim().is_empty() {
        return Err("--reason must not be empty".into());
    }
    Ok(())
}

/// FAM-BUG-048: these commands write state the drive also writes, so running
/// one while a driver holds the claim would race it. Refuse instead.
fn refuse_if_driver_owns(paths: &AppPaths, action: &str) -> Result<(), String> {
    crate::worker_lock::refuse_while_driver_owns(&paths.runtime_dir, action)
        .map_err(|refusal| refusal.to_string())
}

impl OperatorCommand {
    fn authority(&self) -> (&str, &str) {
        match self {
            Self::Rebind { actor, reason, .. }
            | Self::SetPhase { actor, reason, .. }
            | Self::Width { actor, reason, .. } => (actor, reason),
        }
    }

    fn action(&self) -> &'static str {
        match self {
            Self::Rebind { .. } => "rebind a checkpoint",
            Self::SetPhase { .. } => "transition a checkpoint phase",
            Self::Width { .. } => "compute achievable width",
        }
    }
}

pub fn operator(command: OperatorCommand) -> Result<(), String> {
    // Authority first, then the claim, and only then the repository. An
    // operator who mistyped the actor should be told that, not told to stop
    // the daemon or that they are standing in the wrong directory.
    let (actor, reason) = command.authority();
    require_authority(actor, reason)?;
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    refuse_if_driver_owns(&paths, command.action())?;

    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &cwd)?;
    let database = config.database.resolve_path(&paths.data_dir);

    match command {
        OperatorCommand::Rebind {
            prd_id,
            new_base,
            actor,
            reason,
        } => {
            let db = Database::open(&database).map_err(|e| e.to_string())?;
            let checkpoints = CheckpointRepository::new(db.conn());
            let mut checkpoint = checkpoints
                .get(&repository.key, &prd_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no checkpoint for {prd_id} in this repository"))?;
            let old_hash = checkpoint.diff_hash.clone();
            if let Some(base) = new_base {
                println!("rebasing checkpoint base {} -> {base}", checkpoint.base_revision);
                checkpoint.base_revision = base;
            }
            // The same computation freeze and validate use, so a rebind
            // cannot produce a snapshot the drive would compute differently.
            let (evidence, files) = crate::resume::candidate_snapshot(
                Path::new(&checkpoint.worktree_path),
                &checkpoint.base_revision,
            )
            .map_err(|e| e.to_string())?;
            checkpoint.diff_hash = familiar_ai_review::content_hash(&evidence);
            checkpoint.changed_files_json =
                serde_json::to_string(&files).map_err(|e| e.to_string())?;
            let new_hash = checkpoint.diff_hash.clone();
            checkpoints.put(&checkpoint).map_err(|e| e.to_string())?;
            checkpoints
                .record_rebind(
                    &checkpoint.checkpoint_id,
                    &old_hash,
                    &new_hash,
                    &actor,
                    &reason,
                )
                .map_err(|e| e.to_string())?;
            println!("checkpoint {} rebound", checkpoint.checkpoint_id);
            println!("  old diff_hash: {old_hash}");
            println!("  new diff_hash: {new_hash}");
            println!("  manifest files: {}", files.len());
            Ok(())
        }
        OperatorCommand::SetPhase {
            checkpoint_id,
            phase,
            actor,
            reason,
        } => {
            let db = Database::open(&database).map_err(|e| e.to_string())?;
            // The reason travels into the event detail, so the audit trail
            // says who moved the phase and why.
            CheckpointRepository::new(db.conn())
                .transition(&checkpoint_id, &phase, &format!("{actor}: {reason}"))
                .map_err(|e| e.to_string())?;
            println!("{checkpoint_id} -> {phase}");
            Ok(())
        }
        // Authority was validated above; width itself consumes neither.
        OperatorCommand::Width { prds, .. } => {
            let repository_config = config
                .repository(&repository.worktree)
                .map_err(|e| e.to_string())?;
            let discovered = FilesystemBacklogDiscovery
                .discover_with_layout(&repository, &repository_config.layout())
                .map_err(|e| e.to_string())?;
            let subset: Vec<familiar_ai_core::DiscoveredPrd> = discovered
                .into_iter()
                .filter(|prd| {
                    prd.location == familiar_ai_core::PrdLocation::Active
                        && (prds.is_empty()
                            || prds
                                .iter()
                                .any(|want| prd.id.to_string().eq_ignore_ascii_case(want)))
                })
                .collect();
            println!(
                "considering: {}",
                subset
                    .iter()
                    .map(|p| p.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            // Ask the scheduler rather than reimplementing it: an authored
            // width has disagreed with the drive's own admission before
            // (FAM-BUG-010), so this calls the same function the drive calls.
            let report = crate::drive::achievable_width(&repository.worktree, &subset)
                .map_err(|e| e.to_string())?;
            println!(
                "graph_width={} achievable_width={}",
                report.graph_width, report.achievable_width
            );
            if report.conflicts.is_empty() {
                println!("conflicts: none — every pair is scope-disjoint");
            } else {
                for (a, b, reason) in &report.conflicts {
                    println!("conflict: {a} <-> {b}: {reason}");
                }
            }
            Ok(())
        }
    }
}
