//! `familiar-ai backlog` — inspect or roll back the historical backlog
//! bootstrap, and record human-attributed recovery decisions.

use std::path::PathBuf;

use clap::Subcommand;
use familiar_ai_core::{
    load_manifest, resolve_run_prd, validate_graph, validate_recovery_attribution, AppPaths,
    BacklogDiscovery, BacklogRecoveryAction, BacklogStatusStore, FilesystemBacklogDiscovery,
};
use familiar_ai_storage::{Database, SqliteBacklogRepository, SqliteBootstrapRepository};

use super::shared::{database, effective_repository_config, escape_output};

#[derive(Debug, Subcommand)]
pub enum BacklogCommand {
    /// Deterministically check structured PRD metadata migration state. Never writes PRDs.
    MetadataCheck {
        /// Report legacy migration debt and fail only on structured-v1 diagnostics.
        #[arg(long, conflicts_with = "strict")]
        advisory: bool,
        /// Fail on legacy migration debt as well as structured-v1 diagnostics (default).
        #[arg(long, conflicts_with = "advisory")]
        strict: bool,
    },
    Bootstrap {
        #[command(subcommand)]
        command: BootstrapCommand,
    },
    /// Return one claimed PRD to pending without altering its claim or worktree history.
    Release {
        prd_path: PathBuf,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
    /// MANUAL OVERRIDE: bypass PRD-011's normal completion-evidence predicate.
    Complete {
        prd_path: PathBuf,
        /// Mandatory explicit human authority in the form human:<identity>.
        #[arg(long)]
        actor: String,
        /// Mandatory non-empty audit reason for the manual override.
        #[arg(long)]
        reason: String,
    },
    /// Declare, under recorded human authority, that a `pending` PRD was
    /// completed outside Familiar's tracking (a fresh database, a restored
    /// machine, or work merged before Familiar tracked it).
    RecordComplete {
        prd_path: PathBuf,
        /// Mandatory explicit human authority in the form human:<identity>.
        #[arg(long)]
        actor: String,
        /// Mandatory non-empty audit reason for the recorded completion.
        #[arg(long)]
        reason: String,
    },
    /// Complete one reviewed retained checkpoint in a single transaction,
    /// binding the approved candidate hash and the resulting commit. Accepts
    /// an entry left pending (after a release) or in_progress.
    ApproveAndComplete {
        prd_path: PathBuf,
        /// Mandatory explicit human authority in the form human:<identity>.
        #[arg(long)]
        actor: String,
        /// Mandatory non-empty audit reason for the approval.
        #[arg(long)]
        reason: String,
        /// The commit the approved candidate landed as. Defaults to the
        /// repository's current HEAD.
        #[arg(long)]
        commit: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum BootstrapCommand {
    Status,
    Rollback {
        run_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        actor: String,
    },
}

pub fn backlog(command: BacklogCommand) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    let config = effective_repository_config(&paths, &cwd)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|e| e.to_string())?;
    let metadata_mode = match &command {
        BacklogCommand::MetadataCheck { advisory: true, .. } => {
            Some(familiar_ai_core::MetadataCheckMode::Advisory)
        }
        BacklogCommand::MetadataCheck { .. } => Some(familiar_ai_core::MetadataCheckMode::Strict),
        _ => None,
    };
    if let Some(mode) = metadata_mode {
        println!("metadata-check mode={}", mode.as_str());
    }
    let mut layout = repository_config.layout();
    if metadata_mode.is_some() {
        // The checker owns strict/advisory exit semantics. Discovery remains
        // fail-closed for every malformed structured-v1 document in both modes.
        layout.metadata_policy = familiar_ai_core::PrdMetadataPolicy::Incremental;
    }
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&repository, &layout)
        .map_err(|e| e.to_string())?;
    if discovered.is_empty() {
        return Err("backlog is empty".into());
    }
    validate_graph(&discovered).map_err(|e| e.to_string())?;
    if let Some(mode) = metadata_mode {
        let mut legacy = Vec::new();
        for prd in &discovered {
            if prd.metadata.contract_version == Some(1) {
                println!("{}: structured-v1", prd.path);
            } else {
                println!("{}: legacy: add familiar_ai_prd v1 front matter", prd.path);
                legacy.push(prd.path.to_string());
            }
        }
        println!(
            "metadata-check mode={} structured_v1={} legacy={}",
            mode.as_str(),
            discovered.len() - legacy.len(),
            legacy.len()
        );
        if !legacy.is_empty() && mode.legacy_is_failure() {
            return Err(format!(
                "metadata-check mode={}: {} legacy PRD(s) require migration under policy={}: {}",
                mode.as_str(),
                legacy.len(),
                repository_config.prd_metadata_policy,
                legacy.join(", ")
            ));
        }
        return Ok(());
    }
    let mut db = database()?;
    match command {
        BacklogCommand::MetadataCheck { .. } => {
            unreachable!("handled before storage is opened")
        }
        BacklogCommand::Bootstrap {
            command: BootstrapCommand::Status,
        } => {
            let manifest = load_manifest(&repository, &discovered).map_err(|e| e.to_string())?;
            let report = SqliteBootstrapRepository::new(db.conn_mut())
                .status(&repository, manifest.as_ref())
                .map_err(|e| e.to_string())?;
            println!(
                "state={} repository={} run={} manifest={} items={}",
                report.state,
                report.repository_key,
                report.run_id.as_deref().unwrap_or("-"),
                report.canonical_hash.as_deref().unwrap_or("-"),
                report.item_count
            );
        }
        BacklogCommand::Bootstrap {
            command:
                BootstrapCommand::Rollback {
                    run_id,
                    reason,
                    actor,
                },
        } => {
            SqliteBacklogRepository::new(db.conn_mut())
                .reconcile_and_snapshot(&repository, &discovered)
                .map_err(|e| e.to_string())?;
            let result = SqliteBootstrapRepository::new(db.conn_mut())
                .rollback(&repository, &discovered, &run_id, &actor, &reason)
                .map_err(|e| e.to_string())?;
            println!(
                "rollback={} restored={}",
                result.rollback_run_id, result.item_count
            );
        }
        BacklogCommand::Release {
            prd_path,
            actor,
            reason,
        } => recover_backlog(
            &mut db,
            &repository,
            &discovered,
            &prd_path,
            BacklogRecoveryAction::Release,
            &actor,
            &reason,
        )?,
        BacklogCommand::Complete {
            prd_path,
            actor,
            reason,
        } => recover_backlog(
            &mut db,
            &repository,
            &discovered,
            &prd_path,
            BacklogRecoveryAction::ManualCompleteOverride,
            &actor,
            &reason,
        )?,
        BacklogCommand::RecordComplete {
            prd_path,
            actor,
            reason,
        } => record_complete_backlog(
            &mut db,
            &repository,
            &discovered,
            &prd_path,
            &actor,
            &reason,
        )?,
        BacklogCommand::ApproveAndComplete {
            prd_path,
            actor,
            reason,
            commit,
        } => approve_and_complete_backlog(
            &mut db,
            &repository,
            &discovered,
            &prd_path,
            &actor,
            &reason,
            commit.as_deref(),
        )?,
    }
    Ok(())
}

/// PRD-065: complete a reviewed retained checkpoint transactionally, binding
/// the approved candidate hash and the resulting commit. Errors print only
/// commands valid for the entry's current persisted state.
fn approve_and_complete_backlog(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    discovered: &[familiar_ai_core::DiscoveredPrd],
    supplied_path: &std::path::Path,
    actor: &str,
    reason: &str,
    commit: Option<&str>,
) -> Result<(), String> {
    validate_recovery_attribution(BacklogRecoveryAction::ApproveAndComplete, actor, reason)
        .map_err(|e| e.to_string())?;
    let target =
        resolve_run_prd(repository, discovered, supplied_path).map_err(|e| e.to_string())?;
    let checkpoint = familiar_ai_storage::CheckpointRepository::new(db.conn())
        .get(&repository.key, &target.id.to_string())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "no durable checkpoint exists for {}; approve-and-complete acts only on reviewed checkpoints.\nvalid next commands: familiar-ai run {} (fresh attempt), familiar-ai resume {} (inspect partials)",
                target.id,
                target.path,
                target.id
            )
        })?;
    // The checkpoint worktree must still hold the exact approved candidate:
    // that validation is what makes the commit-containment proof below a bind
    // to the APPROVED content rather than to whatever sits on disk (F1).
    let candidate = crate::resume::one(db, &repository.key, &target.id.to_string())?;
    if !candidate.valid {
        return Err(format!(
            "checkpoint for {} is not valid ({}); the worktree no longer holds the approved candidate, so no commit can be provably bound to it.\nvalid next commands: familiar-ai resume {} (inspect), familiar-ai run {} (fresh attempt)",
            target.id,
            candidate.reason.as_deref().unwrap_or("invalid_checkpoint"),
            target.id,
            target.path
        ));
    }
    // The default commit is the MAIN worktree's HEAD (where an approved
    // candidate is merged), never the checkpoint worktree's HEAD, which still
    // sits at the base revision.
    let commit = match commit {
        Some(value) => value.to_owned(),
        None => {
            let output = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repository.worktree)
                .output()
                .map_err(|e| format!("cannot resolve HEAD: {e}"))?;
            if !output.status.success() {
                return Err(format!(
                    "cannot resolve HEAD: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
    };
    // Resolve the commit to a real object and prove it contains the candidate
    // byte for byte. A default of HEAD taken before the candidate was merged
    // fails here instead of durably binding completion to the base revision.
    let commit = crate::resume::verify_commit_contains_candidate(
        &candidate.worktree,
        &commit,
        &candidate.changed_files,
    )?;
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(repository, discovered)
        .map_err(|e| e.to_string())?;
    let result = SqliteBacklogRepository::new(db.conn_mut())
        .approve_and_complete(
            repository,
            &target,
            actor,
            reason,
            &checkpoint.diff_hash,
            &commit,
        )
        .map_err(|error| {
            format!(
                "{error}\n{}",
                backlog_next_commands(db, repository, &target)
            )
        })?;
    println!(
        "backlog approve-and-complete: {} {} -> {} approved_hash={} commit={} actor={} reason={}",
        result.prd.id,
        result.prd.path,
        result.status.as_str(),
        checkpoint.diff_hash,
        commit,
        escape_output(actor.trim()),
        escape_output(reason.trim())
    );
    Ok(())
}

/// The commands valid for one backlog entry's current persisted state.
fn backlog_next_commands(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    target: &familiar_ai_core::DiscoveredPrd,
) -> String {
    let status: Option<String> = familiar_ai_storage::list_backlog_entries(
        db.conn(),
        &repository.key,
        None,
        None,
        usize::MAX,
    )
    .ok()
    .and_then(|rows| {
        rows.into_iter()
            .find(|row| row.prd_path == target.path.as_str())
            .map(|row| row.status)
    });
    let id = &target.id;
    let path = &target.path;
    match status.as_deref() {
        Some("completed") => format!("valid next commands: none — {id} is already completed"),
        Some("blocked") => format!(
            "valid next commands: familiar-ai backlog release {path} --actor ... --reason ..."
        ),
        Some("pending") | Some("in_progress") => format!(
            "valid next commands: familiar-ai backlog approve-and-complete {path} --actor human:<identity> --reason ... (reviewed checkpoint), familiar-ai resume {id} (continue the partial), familiar-ai run {path} (fresh attempt)"
        ),
        _ => format!("valid next commands: familiar-ai run {path} (entry is not tracked yet)"),
    }
}

fn recover_backlog(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    discovered: &[familiar_ai_core::DiscoveredPrd],
    supplied_path: &std::path::Path,
    action: BacklogRecoveryAction,
    actor: &str,
    reason: &str,
) -> Result<(), String> {
    validate_recovery_attribution(action, actor, reason).map_err(|e| e.to_string())?;
    let target =
        resolve_run_prd(repository, discovered, supplied_path).map_err(|e| e.to_string())?;
    let result = SqliteBacklogRepository::new(db.conn_mut())
        .recover(repository, &target, action, actor, reason)
        .map_err(|e| e.to_string())?;
    let actor = actor.trim();
    let reason = reason.trim();
    let label = if action == BacklogRecoveryAction::ManualCompleteOverride {
        " MANUAL OVERRIDE"
    } else {
        ""
    };
    println!(
        "backlog recovery:{label} {} {} in_progress -> {} action={} actor={} reason={}",
        result.prd.id,
        result.prd.path,
        result.status.as_str(),
        action.as_str(),
        escape_output(actor),
        escape_output(reason)
    );
    Ok(())
}

fn record_complete_backlog(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    discovered: &[familiar_ai_core::DiscoveredPrd],
    supplied_path: &std::path::Path,
    actor: &str,
    reason: &str,
) -> Result<(), String> {
    validate_recovery_attribution(BacklogRecoveryAction::RecordedComplete, actor, reason)
        .map_err(|e| e.to_string())?;
    let target =
        resolve_run_prd(repository, discovered, supplied_path).map_err(|e| e.to_string())?;
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(repository, discovered)
        .map_err(|e| e.to_string())?;
    let result = SqliteBacklogRepository::new(db.conn_mut())
        .record_complete(repository, discovered, &target, actor, reason)
        .map_err(|e| e.to_string())?;
    let actor = actor.trim();
    let reason = reason.trim();
    println!(
        "backlog recovery: {} {} pending -> {} action={} actor={} reason={}",
        result.prd.id,
        result.prd.path,
        result.status.as_str(),
        BacklogRecoveryAction::RecordedComplete.as_str(),
        escape_output(actor),
        escape_output(reason)
    );
    Ok(())
}
