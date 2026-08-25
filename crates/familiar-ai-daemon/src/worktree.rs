use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeOwnership {
    pub session_id: String,
    pub prd_id: String,
    pub worktree: PathBuf,
    pub created_at: String,
    pub heartbeat_at: String,
    pub state: String,
}

/// A durable isolated worktree. Drop deliberately does not remove it: agent
/// changes are evidence and survive interruption until explicit retirement.
pub struct WorktreeLease {
    ownership_path: PathBuf,
    ownership: WorktreeOwnership,
}

impl WorktreeLease {
    pub fn create(
        repository: &Path,
        state_dir: &Path,
        session_id: &str,
        prd_id: &str,
    ) -> io::Result<Self> {
        let safe_id: String = prd_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let root = state_dir.join("worktrees").join(session_id);
        fs::create_dir_all(&root)?;
        let worktree = root.join(&safe_id);
        let ownership_path = root.join(format!("{safe_id}.ownership.json"));
        let output = Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&worktree)
            .arg("HEAD")
            .current_dir(repository)
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let now = chrono::Utc::now().to_rfc3339();
        let ownership = WorktreeOwnership {
            session_id: session_id.into(),
            prd_id: prd_id.into(),
            worktree,
            created_at: now.clone(),
            heartbeat_at: now,
            state: "owned".into(),
        };
        persist(&ownership_path, &ownership)?;
        Ok(Self {
            ownership_path,
            ownership,
        })
    }

    pub fn path(&self) -> &Path {
        &self.ownership.worktree
    }

    pub fn heartbeat(&mut self) -> io::Result<()> {
        self.ownership.heartbeat_at = chrono::Utc::now().to_rfc3339();
        persist(&self.ownership_path, &self.ownership)
    }

    pub fn mark_retained(&mut self) -> io::Result<()> {
        self.mark_state("retained")
    }

    pub fn mark_state(&mut self, state: &str) -> io::Result<()> {
        self.ownership.state = state.into();
        self.heartbeat()
    }
}

fn persist(path: &Path, ownership: &WorktreeOwnership) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(ownership).map_err(io::Error::other)?;
    fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_isolated_worktree_and_durable_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@example.invalid"],
            vec!["config", "user.name", "Test"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success());
        }
        fs::write(repo.join("file"), "base").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-qm", "base"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        let state = temp.path().join("state");
        let mut lease = WorktreeLease::create(&repo, &state, "session", "PRD-1").unwrap();
        assert_eq!(
            fs::read_to_string(lease.path().join("file")).unwrap(),
            "base"
        );
        lease.mark_retained().unwrap();
        let record: WorktreeOwnership =
            serde_json::from_slice(&fs::read(&lease.ownership_path).unwrap()).unwrap();
        assert_eq!(record.state, "retained");
    }
}
