//! Deterministic checkpoint discovery and validation.  This module deliberately
//! derives no state from an unrecorded worktree: a candidate exists only when
//! durable checkpoint evidence exists.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use familiar_ai_core::{
    validate_graph, AppPaths, BacklogDiscovery, Config, FilesystemBacklogDiscovery,
};
use familiar_ai_storage::{
    CheckpointRepository, Database, DriverRepository, ExecutionCheckpoint,
    ExecutionHistoryRepository,
};
use std::collections::{BTreeMap, BTreeSet};

use crate::worktree::WorktreeOwnership;

/// Shared resume orchestration. The adapter receives renderable lines and a
/// callback only for the explicitly interactive human-review surface.
pub fn execute_configured<F>(prd: &str, dry_run: bool, mut review: F) -> Result<Vec<String>, String>
where
    F: FnMut(
        crate::run::RunError,
        &Path,
        &Config,
        &AppPaths,
        &crate::run::AgentSet<'_>,
    ) -> Result<(), crate::run::RunError>,
{
    let paths = AppPaths::resolve().map_err(|error| error.to_string())?;
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let config = crate::config_cli::effective_config_for_repository(
        &crate::config_cli::ConfigContext {
            config_path: paths.config_dir.join("config.toml"),
            data_dir: paths.data_dir.clone(),
        },
        &current,
    )?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|error| error.to_string())?;
    let _ownership = if dry_run {
        None
    } else {
        Some(
            crate::worker_lock::WorkerLock::acquire_repository(&paths.runtime_dir, &repository.key)
                .map_err(|error| {
                    format!("cannot acquire mutating orchestrator ownership: {error}")
                })?,
        )
    };
    let db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|error| error.to_string())?;
    if !dry_run {
        db.run_migrations().map_err(|error| error.to_string())?;
    }
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|error| error.to_string())?;
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&repository, &repository_config.layout())
        .map_err(|error| error.to_string())?;
    validate_graph(&discovered).map_err(|error| error.to_string())?;
    let dependencies = discovered
        .iter()
        .map(|entry| {
            (
                entry.id.to_string(),
                entry.dependencies.iter().map(ToString::to_string).collect(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut completed = discovered
        .iter()
        .filter(|entry| entry.location == familiar_ai_core::PrdLocation::Archived)
        .map(|entry| entry.id.to_string())
        .collect::<BTreeSet<_>>();
    completed.extend(
        familiar_ai_storage::OrchestrationRepository::new(db.conn())
            .terminal_prds(&repository.key)
            .map_err(|error| error.to_string())?,
    );
    let active_prds = discovered
        .iter()
        .filter(|entry| entry.location == familiar_ai_core::PrdLocation::Active)
        .map(|entry| (entry.id.to_string(), entry.path.to_string()))
        .collect::<BTreeMap<_, _>>();
    let candidates = if prd == "all" {
        discover_with_legacy(
            &db,
            &repository.key,
            &paths.state_dir,
            &active_prds,
            !dry_run,
        )?
    } else {
        vec![one(&db, &repository.key, prd)?]
    };
    let mut output = render(&candidates)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let (waves, blocked) = if prd == "all" {
        plan_waves(
            &candidates,
            &dependencies,
            &completed,
            config.driver.max_concurrency,
        )
    } else if candidates[0].valid {
        (vec![vec![0]], Vec::new())
    } else {
        (
            Vec::new(),
            vec![(0, candidates[0].reason.clone().unwrap_or_default())],
        )
    };
    for (wave, indexes) in waves.iter().enumerate() {
        output.push(format!(
            "wave={}\t{}",
            wave + 1,
            indexes
                .iter()
                .map(|index| candidates[*index].prd_id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    for (index, reason) in &blocked {
        output.push(format!(
            "blocked\t{}\treason={reason}",
            candidates[*index].prd_id
        ));
    }
    if dry_run {
        return Ok(output);
    }
    if let Some(invalid) = candidates.iter().find(|candidate| !candidate.valid) {
        if prd != "all" {
            return Err(format!(
                "{}: {}",
                invalid.prd_id,
                invalid.reason.as_deref().unwrap_or("invalid_checkpoint")
            ));
        }
    }
    let (implementation_entry, reviewer_entry) = crate::run::resolved_agent_entries(&config)?;
    let implementation = crate::run::build_agent(&implementation_entry);
    let reviewer = crate::run::build_agent(&reviewer_entry);
    let remediation = crate::run::build_agent(&crate::run::resolved_remediation_entry(&config)?);
    let agents = crate::run::AgentSet {
        implementation: implementation.as_ref(),
        reviewer: reviewer.as_ref(),
        remediation: remediation.as_ref(),
    };
    let mut failures = blocked
        .into_iter()
        .map(|(index, reason)| format!("{}: {reason}", candidates[index].prd_id))
        .collect::<Vec<_>>();
    let mut failed_prds = BTreeSet::new();
    for wave in waves {
        let runnable = wave
            .into_iter()
            .filter(|index| {
                let failed_dependencies = dependencies
                    .get(&candidates[*index].prd_id)
                    .into_iter()
                    .flatten()
                    .filter(|dependency| failed_prds.contains(*dependency))
                    .cloned()
                    .collect::<Vec<_>>();
                if failed_dependencies.is_empty() {
                    true
                } else {
                    failures.push(format!(
                        "{}: dependency_failed: {}",
                        candidates[*index].prd_id,
                        failed_dependencies.join(",")
                    ));
                    failed_prds.insert(candidates[*index].prd_id.clone());
                    false
                }
            })
            .collect::<Vec<_>>();
        let results = std::thread::scope(|scope| {
            runnable
                .iter()
                .map(|index| {
                    let candidate = &candidates[*index];
                    scope.spawn(|| {
                        (
                            candidate.prd_id.clone(),
                            candidate.worktree.clone(),
                            // Repository identity (backlog discovery and the
                            // completion target) must come from the PRIMARY
                            // checkout; the fn reads candidate content from
                            // the checkpoint's own worktree. Passing the
                            // worktree here made completion compare a
                            // self-amended PRD against its claim-time hash —
                            // unlandable forever (FAM-BUG-040).
                            crate::run::resume_implemented_checkpoint(
                                &repository.worktree,
                                &candidate.prd_id,
                                &agents,
                                &config,
                                &paths,
                            ),
                        )
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join())
                .collect::<Vec<_>>()
        });
        for result in results {
            match result {
                Ok((id, worktree, Ok(_))) => {
                    // PRD-077: a finished candidate lands into the current
                    // branch through the same merge machinery drive uses — no
                    // manual Git operations.
                    match land_candidate(&repository.worktree, &worktree, &id) {
                        Ok(merged) => output.push(format!("landed\t{id}\t{merged}")),
                        Err(error) => {
                            failed_prds.insert(id.clone());
                            failures.push(format!("{id}: landing_failed: {error}"));
                        }
                    }
                }
                Ok((id, worktree, Err(error))) => {
                    if let Err(error) = review(error, &worktree, &config, &paths, &agents) {
                        failed_prds.insert(id.clone());
                        failures.push(format!("{id}: {error}"));
                    }
                }
                Err(_) => failures.push("resume_worker_panicked".into()),
            }
        }
    }
    if failures.is_empty() {
        Ok(output)
    } else {
        Err(failures.join("; "))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeCandidate {
    pub checkpoint_id: String,
    pub prd_id: String,
    pub prd_path: String,
    pub phase: String,
    pub worktree: PathBuf,
    pub changed_files: Vec<String>,
    pub valid: bool,
    pub reason: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub fn freeze_implementation(
    db: &Database,
    repository_key: &str,
    prd_id: &str,
    prd_path: &str,
    execution_id: &str,
    worktree: &Path,
    agent_identity: &str,
    usage_json: String,
    pending_review: bool,
) -> Result<Vec<String>, String> {
    let base_revision = git(worktree, &["rev-parse", "HEAD"])?;
    let branch = git(worktree, &["branch", "--show-current"])?;
    let (diff, manifest) = snapshot(worktree, &base_revision)?;
    let checkpoint = ExecutionCheckpoint {
        checkpoint_id: format!("checkpoint-{execution_id}"),
        repository_key: repository_key.into(),
        prd_id: prd_id.into(),
        prd_path: prd_path.into(),
        execution_id: Some(execution_id.into()),
        phase: if pending_review {
            "implemented_pending_review"
        } else {
            "implemented"
        }
        .into(),
        base_revision,
        worktree_path: worktree.display().to_string(),
        branch_name: (!branch.is_empty()).then_some(branch),
        diff_hash: familiar_ai_review::content_hash(&diff),
        changed_files_json: serde_json::to_string(&manifest).map_err(|e| e.to_string())?,
        agent_identity: agent_identity.into(),
        usage_json,
        test_evidence_json: r#"{"status":"unknown"}"#.into(),
        invalid_reason: None,
    };
    CheckpointRepository::new(db.conn())
        .put(&checkpoint)
        .map_err(|e| e.to_string())?;
    Ok(manifest)
}

/// PRD-077 (FAM-BUG-018): an operator who reviews a preserved candidate and
/// commits it in its own worktree must not invalidate the checkpoint. When a
/// stale-base candidate has exactly the committed-candidate shape — HEAD's
/// single parent is the recorded base and the working tree is clean — the
/// checkpoint is rebound to the post-commit snapshot and resume proceeds.
fn rebind_operator_commit(
    db: &Database,
    checkpoint: &ExecutionCheckpoint,
) -> Result<Option<ResumeCandidate>, String> {
    let worktree = PathBuf::from(&checkpoint.worktree_path);
    if !worktree.is_dir() {
        return Ok(None);
    }
    let head = git(&worktree, &["rev-parse", "HEAD"])?;
    if head == checkpoint.base_revision {
        return Ok(None);
    }
    let parent = match git(&worktree, &["rev-parse", "HEAD^"]) {
        Ok(parent) => parent,
        Err(_) => return Ok(None),
    };
    if parent != checkpoint.base_revision
        || git(&worktree, &["rev-parse", "--verify", "--quiet", "HEAD^2"]).is_ok()
    {
        return Ok(None);
    }
    let dirty = git(&worktree, &["status", "--porcelain"])?;
    if !dirty.is_empty() {
        return Ok(None);
    }
    let (diff, manifest) = snapshot(&worktree, &checkpoint.base_revision)?;
    let mut rebound = checkpoint.clone();
    rebound.diff_hash = familiar_ai_review::content_hash(&diff);
    rebound.changed_files_json = serde_json::to_string(&manifest).map_err(|e| e.to_string())?;
    CheckpointRepository::new(db.conn())
        .put(&rebound)
        .map_err(|e| e.to_string())?;
    Ok(Some(ResumeCandidate {
        checkpoint_id: rebound.checkpoint_id,
        prd_id: rebound.prd_id,
        prd_path: rebound.prd_path,
        phase: rebound.phase,
        worktree,
        changed_files: manifest,
        valid: true,
        reason: Some("rebound_operator_commit".into()),
    }))
}

pub fn discover(db: &Database, repository_key: &str) -> Result<Vec<ResumeCandidate>, String> {
    let checkpoints = CheckpointRepository::new(db.conn());
    if !checkpoints.schema_available().map_err(|e| e.to_string())? {
        return Ok(Vec::new());
    }
    let terminal = familiar_ai_storage::OrchestrationRepository::new(db.conn())
        .terminal_prds(repository_key)
        .map_err(|e| e.to_string())?;
    checkpoints
        .resumable(repository_key)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|checkpoint| !terminal.contains(&checkpoint.prd_id))
        .map(|checkpoint| {
            let candidate = validate(checkpoint.clone())?;
            if !candidate.valid
                && candidate
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.starts_with("stale_base"))
            {
                if let Some(rebound) = rebind_operator_commit(db, &checkpoint)? {
                    return Ok(rebound);
                }
            }
            Ok(candidate)
        })
        .collect()
}

/// Discover native checkpoints plus pre-checkpoint retained worktrees. Legacy
/// work is eligible only when durable ownership, a matching driver attempt,
/// and a successful execution-history record all agree. Dirty files alone are
/// never evidence of a completed implementation.
pub fn discover_with_legacy(
    db: &Database,
    repository_key: &str,
    state_dir: &Path,
    active_prds: &BTreeMap<String, String>,
    import: bool,
) -> Result<Vec<ResumeCandidate>, String> {
    let terminal = familiar_ai_storage::OrchestrationRepository::new(db.conn())
        .terminal_prds(repository_key)
        .map_err(|e| e.to_string())?;
    let mut candidates = discover(db, repository_key)?
        .into_iter()
        .filter(|candidate| active_prds.contains_key(&candidate.prd_id))
        .collect::<Vec<_>>();
    let existing = candidates
        .iter()
        .map(|candidate| candidate.prd_id.clone())
        .collect::<BTreeSet<_>>();
    for ownership_path in ownership_files(&state_dir.join("worktrees"))? {
        let bytes = fs::read(&ownership_path)
            .map_err(|error| format!("cannot read {}: {error}", ownership_path.display()))?;
        let ownership: WorktreeOwnership = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid {}: {error}", ownership_path.display()))?;
        let Some(prd_path) = active_prds.get(&ownership.prd_id) else {
            continue;
        };
        if existing.contains(&ownership.prd_id) {
            continue;
        }
        // FAM-BUG-016: a preserved worktree for a PRD that is already
        // completed/integrated is historical evidence, never resumable work.
        if terminal.contains(&ownership.prd_id) {
            continue;
        }
        let attempt = DriverRepository::new(db.conn())
            .latest_attempt_for_prd(repository_key, &ownership.prd_id)
            .map_err(|error| error.to_string())?;
        let evidence = attempt
            .as_ref()
            .and_then(|attempt| attempt.execution_id.as_deref())
            .map(|execution_id| {
                ExecutionHistoryRepository::new(db.conn())
                    .get(execution_id)
                    .map_err(|error| error.to_string())
            })
            .transpose()?
            .flatten();
        let successful = evidence.as_ref().is_some_and(|record| {
            record.outcome == "succeeded" && record.exit_code == Some(0) && record.signal.is_none()
        });
        if !successful {
            candidates.push(legacy_invalid(
                &ownership,
                prd_path,
                "legacy_checkpoint_absent: implementation success is not durably proven",
            ));
            continue;
        }
        if !ownership.worktree.is_dir() {
            candidates.push(legacy_invalid(
                &ownership,
                prd_path,
                &format!("missing_worktree: {}", ownership.worktree.display()),
            ));
            continue;
        }
        let execution_id = attempt.and_then(|attempt| attempt.execution_id).unwrap();
        let record = evidence.unwrap();
        let base_revision = git(&ownership.worktree, &["rev-parse", "HEAD"])?;
        let branch = git(&ownership.worktree, &["branch", "--show-current"])?;
        let (diff, files) = snapshot(&ownership.worktree, &base_revision)?;
        let checkpoint = ExecutionCheckpoint {
            checkpoint_id: format!("legacy-checkpoint-{execution_id}"),
            repository_key: repository_key.into(),
            prd_id: ownership.prd_id.clone(),
            prd_path: prd_path.clone(),
            execution_id: Some(execution_id),
            phase: "implemented_pending_review".into(),
            base_revision,
            worktree_path: ownership.worktree.display().to_string(),
            branch_name: (!branch.is_empty()).then_some(branch),
            diff_hash: familiar_ai_review::content_hash(&diff),
            changed_files_json: serde_json::to_string(&files).map_err(|e| e.to_string())?,
            agent_identity: record.agent,
            usage_json: serde_json::json!({
                "input_tokens": record.input_tokens,
                "output_tokens": record.output_tokens,
                "cached_tokens": record.cached_tokens,
                "total_tokens": record.total_tokens,
                "estimated_cost_microusd": record.estimated_cost_microusd,
                "provenance": "legacy_execution_history"
            })
            .to_string(),
            test_evidence_json: r#"{"status":"unknown","provenance":"legacy"}"#.into(),
            invalid_reason: None,
        };
        if import {
            CheckpointRepository::new(db.conn())
                .put(&checkpoint)
                .map_err(|error| error.to_string())?;
        }
        let mut candidate = validate(checkpoint)?;
        candidate.reason = Some("compatibility=legacy_execution_history".into());
        candidates.push(candidate);
    }
    candidates.sort_by(|left, right| {
        left.prd_id
            .cmp(&right.prd_id)
            .then(left.checkpoint_id.cmp(&right.checkpoint_id))
    });
    Ok(candidates)
}

/// PRD-077: merge a finished candidate into the repository's current branch.
/// Commits the candidate in its worktree if needed, merges via the drive
/// merge machinery, and fast-forwards the checked-out branch — failing
/// closed if the operator's tree moved underneath.
fn land_candidate(
    repository_worktree: &Path,
    candidate_worktree: &Path,
    prd_id: &str,
) -> Result<String, String> {
    let dirty = git(candidate_worktree, &["status", "--porcelain"])?;
    if !dirty.is_empty() {
        git(candidate_worktree, &["add", "-A"])?;
        git(
            candidate_worktree,
            &["commit", "-qm", &format!("{prd_id}: resumed candidate")],
        )?;
    }
    let candidate = git(candidate_worktree, &["rev-parse", "HEAD"])?;
    let prior = git(repository_worktree, &["rev-parse", "HEAD"])?;
    if git(
        repository_worktree,
        &["merge-base", "--is-ancestor", &candidate, &prior],
    )
    .is_ok()
    {
        return Ok(prior);
    }
    let merged = crate::drive::merge_candidate(repository_worktree, &prior, &candidate)
        .map_err(|error| format!("integration failed: {error}"))?;
    git(repository_worktree, &["merge", "--ff-only", &merged])
        .map_err(|error| format!("cannot fast-forward the checked-out branch: {error}"))?;
    Ok(merged)
}

fn ownership_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("cannot scan {}: {error}", directory.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".ownership.json"))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn legacy_invalid(ownership: &WorktreeOwnership, prd_path: &str, reason: &str) -> ResumeCandidate {
    ResumeCandidate {
        checkpoint_id: format!("legacy-{}-{}", ownership.session_id, ownership.prd_id),
        prd_id: ownership.prd_id.clone(),
        prd_path: prd_path.into(),
        phase: "invalid_checkpoint".into(),
        worktree: ownership.worktree.clone(),
        changed_files: Vec::new(),
        valid: false,
        reason: Some(reason.into()),
    }
}

pub fn one(db: &Database, repository_key: &str, prd_id: &str) -> Result<ResumeCandidate, String> {
    let checkpoints = CheckpointRepository::new(db.conn());
    if !checkpoints.schema_available().map_err(|e| e.to_string())? {
        return Err(format!("no durable checkpoint for {prd_id}"));
    }
    let terminal = familiar_ai_storage::OrchestrationRepository::new(db.conn())
        .terminal_prds(repository_key)
        .map_err(|e| e.to_string())?;
    if terminal.contains(prd_id) {
        return Err(format!(
            "{prd_id} is already completed and integrated; its preserved worktree is historical evidence, not resumable work"
        ));
    }
    let checkpoint = checkpoints
        .get(repository_key, prd_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no durable checkpoint for {prd_id}"))?;
    let candidate = validate(checkpoint.clone())?;
    if !candidate.valid
        && candidate
            .reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("stale_base"))
    {
        if let Some(rebound) = rebind_operator_commit(db, &checkpoint)? {
            return Ok(rebound);
        }
    }
    Ok(candidate)
}

fn validate(c: ExecutionCheckpoint) -> Result<ResumeCandidate, String> {
    let path = PathBuf::from(&c.worktree_path);
    let invalid = |reason: String| {
        Ok(ResumeCandidate {
            checkpoint_id: c.checkpoint_id.clone(),
            prd_id: c.prd_id.clone(),
            prd_path: c.prd_path.clone(),
            phase: c.phase.clone(),
            worktree: path.clone(),
            changed_files: Vec::new(),
            valid: false,
            reason: Some(reason),
        })
    };
    if !path.is_dir() {
        return invalid(format!("missing_worktree: {}", path.display()));
    }
    let head = git(&path, &["rev-parse", "HEAD"])?;
    if head != c.base_revision {
        return invalid(format!(
            "stale_base: expected={} actual={head}",
            c.base_revision
        ));
    }
    let (diff, actual_files) = snapshot(&path, &c.base_revision)?;
    let actual = familiar_ai_review::content_hash(&diff);
    if actual != c.diff_hash {
        return invalid(format!(
            "hash_mismatch: expected={} actual={actual}",
            c.diff_hash
        ));
    }
    let expected: Vec<String> = serde_json::from_str(&c.changed_files_json)
        .map_err(|e| format!("invalid_manifest: {e}"))?;
    if actual_files != expected {
        return invalid(format!(
            "changed_file_manifest_mismatch: expected={} actual={}",
            c.changed_files_json,
            serde_json::to_string(&actual_files).unwrap()
        ));
    }
    Ok(ResumeCandidate {
        checkpoint_id: c.checkpoint_id,
        prd_id: c.prd_id,
        prd_path: c.prd_path,
        phase: c.phase,
        worktree: path,
        changed_files: expected,
        valid: true,
        reason: None,
    })
}

/// Partition valid resumptions into deterministic dependency/conflict waves.
/// Dependencies name prerequisite PRD ids. Candidates whose active prerequisite
/// is neither already completed nor in the candidate set are returned blocked.
pub fn plan_waves(
    candidates: &[ResumeCandidate],
    dependencies: &BTreeMap<String, Vec<String>>,
    completed: &BTreeSet<String>,
    max_concurrency: usize,
) -> (Vec<Vec<usize>>, Vec<(usize, String)>) {
    let mut remaining: BTreeSet<usize> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| candidate.valid.then_some(index))
        .collect();
    let mut done = completed.clone();
    let mut waves = Vec::new();
    let mut blocked = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| !candidate.valid)
        .map(|(index, candidate)| {
            (
                index,
                candidate
                    .reason
                    .clone()
                    .unwrap_or_else(|| "invalid_checkpoint".into()),
            )
        })
        .collect::<Vec<_>>();
    let ceiling = max_concurrency.max(1);
    loop {
        let ready: Vec<_> = remaining
            .iter()
            .copied()
            .filter(|index| {
                dependencies
                    .get(&candidates[*index].prd_id)
                    .into_iter()
                    .flatten()
                    .all(|dependency| done.contains(dependency))
            })
            .collect();
        if ready.is_empty() {
            break;
        }
        let mut wave: Vec<usize> = Vec::new();
        for index in ready {
            if wave.len() == ceiling {
                break;
            }
            if wave.iter().all(|other| {
                !conflicts(
                    &candidates[index].changed_files,
                    &candidates[*other].changed_files,
                )
            }) {
                wave.push(index);
            }
        }
        if wave.is_empty() {
            break;
        }
        for index in &wave {
            remaining.remove(index);
            done.insert(candidates[*index].prd_id.clone());
        }
        waves.push(wave);
    }
    for index in remaining {
        let unmet = dependencies
            .get(&candidates[index].prd_id)
            .into_iter()
            .flatten()
            .filter(|dependency| !done.contains(*dependency))
            .cloned()
            .collect::<Vec<_>>();
        let reason = if unmet.is_empty() {
            "conflict_planning_stalled".into()
        } else {
            format!("dependency_blocked: {}", unmet.join(","))
        };
        blocked.push((index, reason));
    }
    blocked.sort_by_key(|(index, _)| *index);
    (waves, blocked)
}

fn conflicts(left: &[String], right: &[String]) -> bool {
    left.iter().any(|a| {
        right
            .iter()
            .any(|b| a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/")))
    })
}

/// PRD-065 (review F1): resolve `commit` to a real commit object and prove it
/// contains the validated candidate the checkpoint worktree holds — every
/// file in the candidate's changed-file manifest has byte-identical content
/// in the commit's tree, and a file the candidate deletes is absent from it.
/// Returns the resolved full commit id. Callers must validate the checkpoint
/// first: this binds the commit to the worktree's content, and only a
/// validated checkpoint proves that content is the approved candidate.
pub fn verify_commit_contains_candidate(
    worktree: &Path,
    commit: &str,
    changed_files: &[String],
) -> Result<String, String> {
    let resolved = git(
        worktree,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{commit}^{{commit}}"),
        ],
    )
    .map_err(|error| format!("'{commit}' does not name a commit object: {error}"))?;
    for path in changed_files {
        let candidate = match fs::read(worktree.join(path)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("cannot read candidate file {path}: {error}")),
        };
        let committed = git_bytes(
            worktree,
            &["cat-file", "blob", &format!("{resolved}:{path}")],
        )
        .ok();
        match (candidate, committed) {
            (None, None) => {}
            (Some(ours), Some(theirs)) if ours == theirs => {}
            (Some(_), None) => {
                return Err(format!(
                    "commit {resolved} does not contain approved candidate file '{path}'"
                ))
            }
            (None, Some(_)) => {
                return Err(format!(
                    "approved candidate deletes '{path}' but commit {resolved} still contains it"
                ))
            }
            (Some(_), Some(_)) => {
                return Err(format!(
                    "approved candidate file '{path}' differs from its content in commit {resolved}"
                ))
            }
        }
    }
    Ok(resolved)
}

/// PRD-077: the candidate snapshot (diff bytes + changed-file manifest) for
/// a worktree against its base — the same computation `freeze_implementation`
/// and `validate` use, exposed so remediation can rebind the checkpoint.
pub fn candidate_snapshot(
    worktree: &Path,
    base_revision: &str,
) -> Result<(Vec<u8>, Vec<String>), String> {
    snapshot(worktree, base_revision)
}

fn snapshot(path: &Path, base: &str) -> Result<(Vec<u8>, Vec<String>), String> {
    let mut evidence = git_bytes(path, &["diff", "--binary", "--no-ext-diff", base, "--"])?;
    let tracked = git(path, &["diff", "--name-only", "--no-ext-diff", base, "--"])?;
    let untracked = git_bytes(path, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut files: Vec<String> = tracked
        .lines()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect();
    for raw in untracked.split(|byte| *byte == 0).filter(|v| !v.is_empty()) {
        let name =
            String::from_utf8(raw.to_vec()).map_err(|e| format!("git_path_not_utf8: {e}"))?;
        files.push(name.clone());
        evidence.extend_from_slice(b"\0untracked\0");
        evidence.extend_from_slice(name.as_bytes());
        evidence.push(0);
        evidence.extend_from_slice(
            &std::fs::read(path.join(&name))
                .map_err(|e| format!("cannot read untracked {name}: {e}"))?,
        );
    }
    files.sort();
    files.dedup();
    Ok((evidence, files))
}

fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    let bytes = git_bytes(path, args)?;
    String::from_utf8(bytes)
        .map(|s| s.trim().to_owned())
        .map_err(|e| format!("git_output_not_utf8: {e}"))
}
fn git_bytes(path: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|e| format!("git_unavailable: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git_failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

pub fn render(candidates: &[ResumeCandidate]) -> String {
    let mut out = String::new();
    for c in candidates {
        use std::fmt::Write;
        let _ = writeln!(
            out,
            "{}\tphase={}\tstatus={}\tworktree={}{}",
            c.prd_id,
            c.phase,
            if c.valid {
                "resumable"
            } else {
                "invalid_checkpoint"
            },
            c.worktree.display(),
            c.reason
                .as_ref()
                .map(|r| format!("\treason={r}"))
                .unwrap_or_default()
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_storage::{
        DriverRepository, ExecutionFinalization, ExecutionHistoryRepository, ExecutionStart,
    };

    fn command(root: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn checkpoint_detects_manual_changes_and_preserves_unknown_usage() {
        let root = tempfile::tempdir().unwrap();
        command(root.path(), &["init", "-q"]);
        command(
            root.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        command(root.path(), &["config", "user.name", "Test"]);
        std::fs::write(root.path().join("tracked"), "before").unwrap();
        command(root.path(), &["add", "tracked"]);
        command(root.path(), &["commit", "-qm", "base"]);
        std::fs::write(root.path().join("tracked"), "after").unwrap();
        std::fs::write(root.path().join("new"), "new").unwrap();
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        freeze_implementation(
            &db,
            "repo",
            "PRD-900",
            "docs/prds/PRD-900.md",
            "exec",
            root.path(),
            "agent",
            r#"{"total_tokens":null}"#.into(),
            true,
        )
        .unwrap();
        let candidate = one(&db, "repo", "PRD-900").unwrap();
        assert!(candidate.valid);
        let stored = CheckpointRepository::new(db.conn())
            .get("repo", "PRD-900")
            .unwrap()
            .unwrap();
        assert_eq!(stored.phase, "implemented_pending_review");
        assert!(stored.usage_json.contains("null"));
        std::fs::write(root.path().join("new"), "manually altered").unwrap();
        let candidate = one(&db, "repo", "PRD-900").unwrap();
        assert!(!candidate.valid);
        assert!(candidate.reason.unwrap().starts_with("hash_mismatch:"));
    }

    /// PRD-065 review F1: the reviewer's required rejections — a nonexistent
    /// commit, the unchanged base commit, and a different valid commit all
    /// fail; only a commit actually containing the candidate binds.
    #[test]
    fn commit_containment_accepts_only_the_candidate_commit() {
        let root = tempfile::tempdir().unwrap();
        command(root.path(), &["init", "-q"]);
        command(
            root.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        command(root.path(), &["config", "user.name", "Test"]);
        std::fs::write(root.path().join("tracked"), "before").unwrap();
        std::fs::write(root.path().join("doomed"), "delete me").unwrap();
        command(root.path(), &["add", "tracked", "doomed"]);
        command(root.path(), &["commit", "-qm", "base"]);
        let base = git(root.path(), &["rev-parse", "HEAD"]).unwrap();

        // The candidate: modify tracked, delete doomed, add untracked.
        std::fs::write(root.path().join("tracked"), "after").unwrap();
        std::fs::remove_file(root.path().join("doomed")).unwrap();
        std::fs::write(root.path().join("fresh"), "new file").unwrap();
        let manifest = vec!["doomed".to_string(), "fresh".into(), "tracked".into()];

        // Create the candidate commit, then move HEAD back so the worktree
        // still holds the candidate as dirty state (the checkpoint shape).
        command(root.path(), &["add", "-A"]);
        command(root.path(), &["commit", "-qm", "candidate"]);
        let candidate_commit = git(root.path(), &["rev-parse", "HEAD"]).unwrap();
        command(root.path(), &["reset", "-q", "--mixed", "HEAD~1"]);

        // A different valid commit: same shape as the candidate (doomed
        // deleted, fresh present) but divergent tracked content, so rejection
        // is specifically by content comparison.
        std::fs::write(root.path().join("tracked"), "divergent").unwrap();
        command(root.path(), &["add", "-A"]);
        command(root.path(), &["commit", "-qm", "divergent"]);
        let divergent = git(root.path(), &["rev-parse", "HEAD"]).unwrap();
        command(root.path(), &["reset", "-q", "--mixed", "HEAD~1"]);
        std::fs::write(root.path().join("tracked"), "after").unwrap();

        // The candidate commit binds and resolves to its full id.
        assert_eq!(
            verify_commit_contains_candidate(root.path(), &candidate_commit, &manifest).unwrap(),
            candidate_commit
        );
        // The unchanged base commit is rejected: it lacks the candidate. The
        // first divergence found is `doomed`, which the base still contains.
        let error = verify_commit_contains_candidate(root.path(), &base, &manifest).unwrap_err();
        assert!(error.contains("doomed"), "{error}");
        // A different valid commit is rejected by content.
        let error =
            verify_commit_contains_candidate(root.path(), &divergent, &manifest).unwrap_err();
        assert!(
            error.contains("differs") || error.contains("does not contain"),
            "{error}"
        );
        // A nonexistent commit is rejected outright.
        let error = verify_commit_contains_candidate(
            root.path(),
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            &manifest,
        )
        .unwrap_err();
        assert!(error.contains("does not name a commit"), "{error}");
        // A commit that resurrects a candidate-deleted file is rejected: the
        // base still contains `doomed`, and the message names it when the
        // other files happen to match. Build one: candidate content but with
        // doomed still present.
        std::fs::write(root.path().join("doomed"), "delete me").unwrap();
        command(root.path(), &["add", "-A"]);
        command(root.path(), &["commit", "-qm", "kept-doomed"]);
        let kept = git(root.path(), &["rev-parse", "HEAD"]).unwrap();
        command(root.path(), &["reset", "-q", "--mixed", "HEAD~1"]);
        std::fs::remove_file(root.path().join("doomed")).unwrap();
        let error = verify_commit_contains_candidate(root.path(), &kept, &manifest).unwrap_err();
        assert!(error.contains("doomed"), "{error}");
    }

    /// FAM-BUG-016 regression: a checkpoint for a PRD whose durable backlog
    /// status is completed is historical evidence, never resumable work —
    /// including across the zero-padded-file-stem ("PRD-009") versus
    /// canonical-id ("PRD-9") spelling gap that let stale wave-2 worktrees
    /// block wave-3 recovery.
    #[test]
    fn completed_prds_are_suppressed_from_recovery_inventory() {
        use familiar_ai_core::{
            BacklogStatusStore, DiscoveredPrd, PrdId, PrdLocation, RepositoryIdentity,
        };
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repository = RepositoryIdentity {
            worktree: "/tmp/work".into(),
            key: "repo".into(),
        };
        // Zero-padded document path, archived location => durable completed.
        let prd = DiscoveredPrd {
            id: PrdId::new(9),
            number: 9,
            path: familiar_ai_core::RepositoryPath::new("docs/prds/done/PRD-009.md").unwrap(),
            location: PrdLocation::Archived,
            title: "Nine".into(),
            dependencies: vec![],
            metadata: Default::default(),
            content_hash: "abc".into(),
        };
        familiar_ai_storage::SqliteBacklogRepository::new(db.conn_mut())
            .reconcile_and_snapshot(&repository, &[prd])
            .unwrap();
        // A stale checkpoint survives under the CANONICAL id spelling.
        CheckpointRepository::new(db.conn())
            .put(&ExecutionCheckpoint {
                checkpoint_id: "checkpoint-old".into(),
                repository_key: "repo".into(),
                prd_id: "PRD-9".into(),
                prd_path: "docs/prds/PRD-009.md".into(),
                execution_id: Some("exec-old".into()),
                phase: "implemented_pending_review".into(),
                base_revision: "stale".into(),
                worktree_path: "/nonexistent".into(),
                branch_name: None,
                diff_hash: "sha256:old".into(),
                changed_files_json: "[]".into(),
                agent_identity: "agent".into(),
                usage_json: "{}".into(),
                test_evidence_json: "{}".into(),
                invalid_reason: None,
            })
            .unwrap();
        assert!(
            discover(&db, "repo").unwrap().is_empty(),
            "completed PRD's stale checkpoint must be suppressed"
        );
        let error = one(&db, "repo", "PRD-9").unwrap_err();
        assert!(error.contains("already completed"), "{error}");
    }

    /// PRD-077 (FAM-BUG-018): an operator committing the candidate inside its
    /// preserved worktree rebinds the checkpoint instead of invalidating it.
    #[test]
    fn operator_commit_rebinds_checkpoint_instead_of_stale_base() {
        let root = tempfile::tempdir().unwrap();
        command(root.path(), &["init", "-q"]);
        command(
            root.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        command(root.path(), &["config", "user.name", "Test"]);
        std::fs::write(root.path().join("tracked"), "before").unwrap();
        command(root.path(), &["add", "tracked"]);
        command(root.path(), &["commit", "-qm", "base"]);
        std::fs::write(root.path().join("tracked"), "after").unwrap();
        std::fs::write(root.path().join("fresh"), "new file").unwrap();
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        freeze_implementation(
            &db,
            "repo",
            "PRD-901",
            "docs/prds/PRD-901.md",
            "exec-901",
            root.path(),
            "agent",
            "{}".into(),
            false,
        )
        .unwrap();
        // Operator reviews and commits the candidate in place.
        command(root.path(), &["add", "-A"]);
        command(root.path(), &["commit", "-qm", "reviewed candidate"]);
        let candidate = one(&db, "repo", "PRD-901").unwrap();
        assert!(candidate.valid, "{:?}", candidate.reason);
        assert_eq!(candidate.reason.as_deref(), Some("rebound_operator_commit"));
        assert!(candidate
            .changed_files
            .iter()
            .any(|file| file == "tracked" || file == "fresh"));
        // A worktree with EXTRA edits after the commit stays invalid — the
        // rebind accepts exactly the committed-candidate shape.
        std::fs::write(root.path().join("tracked"), "tampered").unwrap();
        let tampered = one(&db, "repo", "PRD-901").unwrap();
        assert!(!tampered.valid);
    }

    /// PRD-077: a finished candidate lands into the checked-out branch with
    /// no manual Git operations, and landing is idempotent.
    #[test]
    fn land_candidate_merges_and_fast_forwards_the_branch() {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("main");
        std::fs::create_dir(&main).unwrap();
        command(&main, &["init", "-q", "-b", "main"]);
        command(&main, &["config", "user.email", "test@example.invalid"]);
        command(&main, &["config", "user.name", "Test"]);
        std::fs::write(main.join("shared"), "base").unwrap();
        command(&main, &["add", "shared"]);
        command(&main, &["commit", "-qm", "base"]);
        let worktree = temp.path().join("candidate");
        command(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "familiar/test/PRD-9",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::write(worktree.join("feature"), "implemented").unwrap();
        let merged = land_candidate(&main, &worktree, "PRD-9").unwrap();
        assert_eq!(git(&main, &["rev-parse", "HEAD"]).unwrap(), merged);
        assert_eq!(
            std::fs::read_to_string(main.join("feature")).unwrap(),
            "implemented"
        );
        // Idempotent: landing again changes nothing.
        let again = land_candidate(&main, &worktree, "PRD-9").unwrap();
        assert_eq!(again, merged);
    }

    #[test]
    fn rendering_is_byte_stable() {
        let candidates = vec![
            ResumeCandidate {
                checkpoint_id: "cp-2".into(),
                prd_id: "PRD-002".into(),
                prd_path: "docs/prds/PRD-002.md".into(),
                phase: "implemented".into(),
                worktree: "/tmp/b".into(),
                changed_files: vec!["src/b.rs".into()],
                valid: true,
                reason: None,
            },
            ResumeCandidate {
                checkpoint_id: "cp-3".into(),
                prd_id: "PRD-003".into(),
                prd_path: "docs/prds/PRD-003.md".into(),
                phase: "blocked".into(),
                worktree: "/tmp/c".into(),
                changed_files: vec![],
                valid: false,
                reason: Some("missing_worktree: /tmp/c".into()),
            },
        ];
        assert_eq!(render(&candidates), render(&candidates));
    }

    #[test]
    fn waves_preserve_dependencies_and_serialize_conflicts() {
        let candidate = |id: &str, file: &str| ResumeCandidate {
            checkpoint_id: format!("cp-{id}"),
            prd_id: id.into(),
            prd_path: format!("docs/prds/{id}.md"),
            phase: "implemented".into(),
            worktree: format!("/tmp/{id}").into(),
            changed_files: vec![file.into()],
            valid: true,
            reason: None,
        };
        let candidates = vec![
            candidate("PRD-001", "src/a.rs"),
            candidate("PRD-002", "src/b.rs"),
            candidate("PRD-003", "src/a.rs"),
        ];
        let dependencies = BTreeMap::from([("PRD-002".into(), vec!["PRD-001".into()])]);
        let (waves, blocked) = plan_waves(&candidates, &dependencies, &BTreeSet::new(), 3);
        assert_eq!(waves, vec![vec![0], vec![1, 2]]);
        assert!(blocked.is_empty());
    }

    #[test]
    fn legacy_import_requires_successful_history_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join("worktrees/session/PRD-1");
        std::fs::create_dir_all(&worktree).unwrap();
        command(&worktree, &["init", "-q"]);
        command(&worktree, &["config", "user.email", "test@example.invalid"]);
        command(&worktree, &["config", "user.name", "Test"]);
        std::fs::write(worktree.join("tracked"), "before").unwrap();
        command(&worktree, &["add", "tracked"]);
        command(&worktree, &["commit", "-qm", "base"]);
        std::fs::write(worktree.join("tracked"), "after").unwrap();
        let ownership = WorktreeOwnership {
            session_id: "session".into(),
            prd_id: "PRD-1".into(),
            worktree: worktree.clone(),
            created_at: "2026-01-01T00:00:00Z".into(),
            heartbeat_at: "2026-01-01T00:00:00Z".into(),
            state: "retained".into(),
        };
        std::fs::write(
            root.path().join("worktrees/session/PRD-1.ownership.json"),
            serde_json::to_vec(&ownership).unwrap(),
        )
        .unwrap();

        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        DriverRepository::new(db.conn())
            .open_session("session", "repo", "{}")
            .unwrap();
        DriverRepository::new(db.conn())
            .record_attempt_started("session", "PRD-1", "docs/prds/PRD-001.md", Some("exec"))
            .unwrap();
        ExecutionHistoryRepository::new(db.conn())
            .insert_running(&ExecutionStart {
                execution_id: "exec".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
                repository: "repo".into(),
                worktree: worktree.display().to_string(),
                git_commit: None,
                prd_path: "docs/prds/PRD-001.md".into(),
                unavailable_fields: BTreeMap::new(),
            })
            .unwrap();
        ExecutionHistoryRepository::new(db.conn())
            .finalize(
                "exec",
                &ExecutionFinalization {
                    ended_at: "2026-01-01T00:01:00Z".into(),
                    outcome: "succeeded".into(),
                    exit_code: Some(0),
                    ..ExecutionFinalization::default()
                },
            )
            .unwrap();
        let active = BTreeMap::from([("PRD-1".into(), "docs/prds/PRD-001.md".into())]);
        let first = discover_with_legacy(&db, "repo", root.path(), &active, true).unwrap();
        let second = discover_with_legacy(&db, "repo", root.path(), &active, true).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert!(second[0].valid);
        let checkpoints = CheckpointRepository::new(db.conn());
        checkpoints
            .transition(&second[0].checkpoint_id, "verified", "checks_passed")
            .unwrap();
        checkpoints
            .transition(&second[0].checkpoint_id, "verified", "checks_passed")
            .unwrap();
        assert_eq!(
            checkpoints.events(&second[0].checkpoint_id).unwrap().len(),
            2
        );
    }
}
