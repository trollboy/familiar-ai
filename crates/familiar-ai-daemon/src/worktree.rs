use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeRecoveryResult {
    pub session_id: String,
    pub prd_id: String,
    pub outcome: String,
    pub reason: String,
    pub recovered_at: String,
}

/// Terminalize filesystem evidence left by a killed worker. Worktrees are
/// deliberately retained; recovery only changes ownership from active to an
/// explicit interrupted result and never deletes user or agent changes.
pub fn recover_incomplete(state_dir: &Path) -> io::Result<usize> {
    let root = state_dir.join("worktrees");
    let sessions = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut recovered = 0;
    for session in sessions {
        let session = session?;
        if !session.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(session.path())? {
            let entry = entry?;
            let path = entry.path();
            if !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".ownership.json"))
            {
                continue;
            }
            let mut ownership: WorktreeOwnership =
                serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?;
            if ownership.state != "owned" {
                continue;
            }
            let now = chrono::Utc::now().to_rfc3339();
            ownership.state = "retained_interrupted".into();
            ownership.heartbeat_at = now.clone();
            persist(&path, &ownership)?;
            let result = WorktreeRecoveryResult {
                session_id: ownership.session_id.clone(),
                prd_id: ownership.prd_id.clone(),
                outcome: "retained".into(),
                reason: "interrupted".into(),
                recovered_at: now,
            };
            let result_path = path.with_file_name(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("worktree.ownership.json")
                    .replace(".ownership.json", ".result.json"),
            );
            let bytes = serde_json::to_vec_pretty(&result).map_err(io::Error::other)?;
            let temporary = result_path.with_extension("json.tmp");
            fs::write(&temporary, bytes)?;
            fs::rename(temporary, result_path)?;
            recovered += 1;
        }
    }
    Ok(recovered)
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

    pub fn ownership_path(&self) -> &Path {
        &self.ownership_path
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

    pub fn start_heartbeat(&self, interval: Duration) -> WorktreeHeartbeatGuard {
        WorktreeHeartbeatGuard::start(self.ownership_path.clone(), interval)
    }
}

fn persist(path: &Path, ownership: &WorktreeOwnership) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(ownership).map_err(io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

pub struct WorktreeHeartbeatGuard {
    stop: mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
    failed: Arc<AtomicBool>,
}

impl WorktreeHeartbeatGuard {
    fn start(ownership_path: PathBuf, interval: Duration) -> Self {
        let (stop, receiver) = mpsc::channel();
        let failed = Arc::new(AtomicBool::new(false));
        let thread_failed = Arc::clone(&failed);
        let handle = thread::spawn(move || {
            while receiver.recv_timeout(interval).is_err() {
                let result = fs::read(&ownership_path)
                    .and_then(|bytes| {
                        serde_json::from_slice::<WorktreeOwnership>(&bytes)
                            .map_err(io::Error::other)
                    })
                    .and_then(|mut ownership| {
                        ownership.heartbeat_at = chrono::Utc::now().to_rfc3339();
                        persist(&ownership_path, &ownership)
                    });
                if result.is_err() {
                    thread_failed.store(true, Ordering::Release);
                    break;
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
            failed,
        }
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

impl Drop for WorktreeHeartbeatGuard {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
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

    #[test]
    fn active_worktree_heartbeat_is_periodic_and_remains_valid_json() {
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
            .args(["add", "file"])
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

        let lease = WorktreeLease::create(&repo, temp.path(), "session", "PRD-2").unwrap();
        let before: WorktreeOwnership =
            serde_json::from_slice(&fs::read(&lease.ownership_path).unwrap()).unwrap();
        let guard = lease.start_heartbeat(Duration::from_millis(10));
        thread::sleep(Duration::from_millis(35));
        assert!(!guard.failed());
        drop(guard);
        let after: WorktreeOwnership =
            serde_json::from_slice(&fs::read(&lease.ownership_path).unwrap()).unwrap();
        assert!(after.heartbeat_at > before.heartbeat_at);
    }

    #[test]
    fn missing_ownership_record_marks_worktree_heartbeat_failed() {
        let temp = tempfile::tempdir().unwrap();
        let ownership_path = temp.path().join("missing.ownership.json");
        let guard = WorktreeHeartbeatGuard::start(ownership_path, Duration::from_millis(5));
        thread::sleep(Duration::from_millis(20));
        assert!(guard.failed());
    }

    #[test]
    fn recovery_terminalizes_owned_worktree_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("worktrees/session");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("PRD-9.ownership.json");
        persist(
            &path,
            &WorktreeOwnership {
                session_id: "session".into(),
                prd_id: "PRD-9".into(),
                worktree: root.join("PRD-9"),
                created_at: "before".into(),
                heartbeat_at: "before".into(),
                state: "owned".into(),
            },
        )
        .unwrap();
        assert_eq!(recover_incomplete(temp.path()).unwrap(), 1);
        assert_eq!(recover_incomplete(temp.path()).unwrap(), 0);
        let ownership: WorktreeOwnership =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(ownership.state, "retained_interrupted");
        let result: WorktreeRecoveryResult =
            serde_json::from_slice(&fs::read(root.join("PRD-9.result.json")).unwrap()).unwrap();
        assert_eq!(result.reason, "interrupted");
    }
}
