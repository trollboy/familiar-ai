//! Deterministic checkpoint discovery and validation.  This module deliberately
//! derives no state from an unrecorded worktree: a candidate exists only when
//! durable checkpoint evidence exists.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use familiar_ai_storage::{
    CheckpointRepository, Database, DriverRepository, ExecutionCheckpoint,
    ExecutionHistoryRepository,
};
use std::collections::{BTreeMap, BTreeSet};

use crate::worktree::WorktreeOwnership;

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
) -> Result<(), String> {
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
        .map_err(|e| e.to_string())
}

pub fn discover(db: &Database, repository_key: &str) -> Result<Vec<ResumeCandidate>, String> {
    let checkpoints = CheckpointRepository::new(db.conn());
    if !checkpoints.schema_available().map_err(|e| e.to_string())? {
        return Ok(Vec::new());
    }
    checkpoints
        .resumable(repository_key)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(validate)
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
    let checkpoint = checkpoints
        .get(repository_key, prd_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no durable checkpoint for {prd_id}"))?;
    validate(checkpoint)
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
