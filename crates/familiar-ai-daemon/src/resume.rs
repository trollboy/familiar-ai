//! Deterministic checkpoint discovery and validation.  This module deliberately
//! derives no state from an unrecorded worktree: a candidate exists only when
//! durable checkpoint evidence exists.
use std::path::{Path, PathBuf};
use std::process::Command;

use familiar_ai_storage::{CheckpointRepository, Database, ExecutionCheckpoint};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeCandidate {
    pub prd_id: String,
    pub phase: String,
    pub worktree: PathBuf,
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
    CheckpointRepository::new(db.conn())
        .resumable(repository_key)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(validate)
        .collect()
}

pub fn one(db: &Database, repository_key: &str, prd_id: &str) -> Result<ResumeCandidate, String> {
    let checkpoint = CheckpointRepository::new(db.conn())
        .get(repository_key, prd_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no durable checkpoint for {prd_id}"))?;
    validate(checkpoint)
}

fn validate(c: ExecutionCheckpoint) -> Result<ResumeCandidate, String> {
    let path = PathBuf::from(&c.worktree_path);
    let invalid = |reason: String| {
        Ok(ResumeCandidate {
            prd_id: c.prd_id.clone(),
            phase: c.phase.clone(),
            worktree: path.clone(),
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
        prd_id: c.prd_id,
        phase: c.phase,
        worktree: path,
        valid: true,
        reason: None,
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
            "PRD-039",
            "docs/prds/PRD-039.md",
            "exec",
            root.path(),
            "agent",
            r#"{"total_tokens":null}"#.into(),
            true,
        )
        .unwrap();
        let candidate = one(&db, "repo", "PRD-039").unwrap();
        assert!(candidate.valid);
        let stored = CheckpointRepository::new(db.conn())
            .get("repo", "PRD-039")
            .unwrap()
            .unwrap();
        assert_eq!(stored.phase, "implemented_pending_review");
        assert!(stored.usage_json.contains("null"));
        std::fs::write(root.path().join("new"), "manually altered").unwrap();
        let candidate = one(&db, "repo", "PRD-039").unwrap();
        assert!(!candidate.valid);
        assert!(candidate.reason.unwrap().starts_with("hash_mismatch:"));
    }

    #[test]
    fn rendering_is_byte_stable() {
        let candidates = vec![
            ResumeCandidate {
                prd_id: "PRD-002".into(),
                phase: "implemented".into(),
                worktree: "/tmp/b".into(),
                valid: true,
                reason: None,
            },
            ResumeCandidate {
                prd_id: "PRD-003".into(),
                phase: "blocked".into(),
                worktree: "/tmp/c".into(),
                valid: false,
                reason: Some("missing_worktree: /tmp/c".into()),
            },
        ];
        assert_eq!(render(&candidates), render(&candidates));
    }
}
