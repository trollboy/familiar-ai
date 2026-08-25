//! Explicit, finite delivery boundary for reviewed isolated worktrees.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use familiar_ai_core::DeliveryConfig;
use serde::{Deserialize, Serialize};
use wait_timeout::ChildExt;

use crate::worktree::WorktreeOwnership;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryJournal {
    pub session_id: String,
    pub prd_id: String,
    pub worktree: PathBuf,
    pub branch: String,
    pub pr_number: Option<u64>,
    pub phase: String,
    pub detail: Option<String>,
    pub updated_at: String,
}

pub trait CommandRunner {
    fn run(&self, directory: &Path, argv: &[String]) -> Result<Output, String>;
}

pub struct ProcessRunner {
    timeout: Duration,
}

impl ProcessRunner {
    fn new(timeout_ms: u64) -> Self {
        Self {
            timeout: Duration::from_millis(timeout_ms),
        }
    }
}

impl CommandRunner for ProcessRunner {
    fn run(&self, directory: &Path, argv: &[String]) -> Result<Output, String> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| "delivery command argv is empty".to_owned())?;
        let mut child = Command::new(program)
            .args(args)
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot launch {program:?}: {error}"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("cannot capture {program:?} stdout"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("cannot capture {program:?} stderr"))?;
        let stdout_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let status = match child
            .wait_timeout(self.timeout)
            .map_err(|error| format!("cannot wait for {program:?}: {error}"))?
        {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let status = child
                    .wait()
                    .map_err(|error| format!("cannot reap timed-out {program:?}: {error}"))?;
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!(
                    "{program:?} exceeded delivery command timeout of {}ms (status {:?})",
                    self.timeout.as_millis(),
                    status.code()
                ));
            }
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| format!("{program:?} stdout reader panicked"))?
            .map_err(|error| format!("cannot read {program:?} stdout: {error}"))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| format!("{program:?} stderr reader panicked"))?
            .map_err(|error| format!("cannot read {program:?} stderr: {error}"))?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

pub fn deliver(ownership_path: &Path, policy: &DeliveryConfig) -> Result<DeliveryJournal, String> {
    deliver_with(
        ownership_path,
        policy,
        &ProcessRunner::new(policy.command_timeout_ms),
    )
}

pub fn deliver_with(
    ownership_path: &Path,
    policy: &DeliveryConfig,
    runner: &dyn CommandRunner,
) -> Result<DeliveryJournal, String> {
    policy.validate()?;
    if !policy.enabled {
        return Err("delivery policy is disabled".into());
    }
    reject_production_commands(policy)?;
    let ownership: WorktreeOwnership = serde_json::from_slice(
        &fs::read(ownership_path)
            .map_err(|error| format!("cannot read ownership record: {error}"))?,
    )
    .map_err(|error| format!("invalid ownership record: {error}"))?;
    if ownership.state != "ready_for_delivery" {
        return Err(format!(
            "worktree is not reviewed and ready for delivery (state={})",
            ownership.state
        ));
    }
    let safe_prd: String = ownership
        .prd_id
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() {
                value
            } else {
                '-'
            }
        })
        .collect();
    let branch = format!("familiar/{}/{safe_prd}", ownership.session_id);
    let journal_path = ownership_path.with_extension("delivery.json");
    let mut journal = DeliveryJournal {
        session_id: ownership.session_id,
        prd_id: ownership.prd_id,
        worktree: ownership.worktree,
        branch: branch.clone(),
        pr_number: None,
        phase: "admitted".into(),
        detail: None,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    persist(&journal_path, &journal)?;

    checked(runner, &journal.worktree, &["git", "switch", "-c", &branch])?;
    checked(runner, &journal.worktree, &["git", "add", "-A"])?;
    let staged = checked(
        runner,
        &journal.worktree,
        &["git", "diff", "--cached", "--name-only"],
    )?;
    let staged_paths = String::from_utf8_lossy(&staged.stdout);
    let migration = staged_paths
        .lines()
        .find(|path| path.to_ascii_lowercase().contains("migration"));
    if let Some(path) = migration {
        return fail_journal(
            &journal_path,
            journal,
            "deployment_blocked",
            format!(
                "migration change {path} has no automatic database rollback; human staging authority required"
            ),
        );
    }
    checked(
        runner,
        &journal.worktree,
        &[
            "git",
            "commit",
            "-m",
            &format!("feat: implement {}", journal.prd_id),
        ],
    )?;
    journal.phase = "committed".into();
    persist(&journal_path, &journal)?;
    checked(
        runner,
        &journal.worktree,
        &["git", "push", "-u", &policy.remote, &branch],
    )?;
    journal.phase = "pushed".into();
    persist(&journal_path, &journal)?;

    let create = argv(&[
        "gh",
        "pr",
        "create",
        "--fill",
        "--base",
        &policy.base,
        "--head",
        &branch,
    ]);
    if let Err(error) = checked_owned(runner, &journal.worktree, &create) {
        journal.detail = Some(format!(
            "PR create returned: {error}; checking for existing PR"
        ));
    }
    let view = checked_owned(
        runner,
        &journal.worktree,
        &argv(&[
            "gh", "pr", "view", &branch, "--json", "number", "--jq", ".number",
        ]),
    )?;
    journal.pr_number = String::from_utf8_lossy(&view.stdout).trim().parse().ok();
    let pr = journal
        .pr_number
        .ok_or_else(|| "GitHub did not return a pull request number".to_owned())?;
    journal.phase = "published".into();
    persist(&journal_path, &journal)?;

    if !policy.auto_merge {
        journal.phase = "awaiting_merge_authority".into();
        persist(&journal_path, &journal)?;
        return Ok(journal);
    }
    if let Err(error) = checked(
        runner,
        &journal.worktree,
        &[
            "gh",
            "pr",
            "checks",
            &pr.to_string(),
            "--watch",
            "--fail-fast",
        ],
    ) {
        comment_blocker(runner, policy, &journal.worktree, pr, &error);
        return fail_journal(&journal_path, journal, "checks_failed", error);
    }
    checked(
        runner,
        &journal.worktree,
        &[
            "gh",
            "pr",
            "merge",
            &pr.to_string(),
            "--merge",
            "--delete-branch",
        ],
    )?;
    journal.phase = "merged".into();
    persist(&journal_path, &journal)?;

    checked_owned(runner, &journal.worktree, &policy.deploy_argv)?;
    journal.phase = "staging_deployed".into();
    persist(&journal_path, &journal)?;
    if let Err(smoke_error) = checked_owned(runner, &journal.worktree, &policy.smoke_argv) {
        let rollback = checked_owned(runner, &journal.worktree, &policy.rollback_argv)
            .map(|_| "rollback passed".to_owned())
            .unwrap_or_else(|error| format!("rollback failed: {error}"));
        let detail = format!("staging smoke failed: {smoke_error}; {rollback}");
        comment_blocker(runner, policy, &journal.worktree, pr, &detail);
        return fail_journal(&journal_path, journal, "staging_rolled_back", detail);
    }
    journal.phase = "staging_verified".into();
    journal.detail = None;
    persist(&journal_path, &journal)?;
    Ok(journal)
}

fn reject_production_commands(policy: &DeliveryConfig) -> Result<(), String> {
    for arg in policy
        .deploy_argv
        .iter()
        .chain(&policy.smoke_argv)
        .chain(&policy.rollback_argv)
    {
        let lower = arg.to_ascii_lowercase();
        if lower == "prod" || lower.contains("production") {
            return Err("production delivery is prohibited".into());
        }
    }
    Ok(())
}

fn argv(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn checked(
    runner: &dyn CommandRunner,
    directory: &Path,
    values: &[&str],
) -> Result<Output, String> {
    checked_owned(runner, directory, &argv(values))
}

fn checked_owned(
    runner: &dyn CommandRunner,
    directory: &Path,
    values: &[String],
) -> Result<Output, String> {
    let output = runner.run(directory, values)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "{:?} exited {:?}: {}",
            values,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn comment_blocker(
    runner: &dyn CommandRunner,
    policy: &DeliveryConfig,
    directory: &Path,
    pr: u64,
    detail: &str,
) {
    if policy.comment_blockers {
        let _ = checked(
            runner,
            directory,
            &["gh", "pr", "comment", &pr.to_string(), "--body", detail],
        );
    }
}

fn fail_journal(
    path: &Path,
    mut journal: DeliveryJournal,
    phase: &str,
    detail: String,
) -> Result<DeliveryJournal, String> {
    journal.phase = phase.into();
    journal.detail = Some(detail.clone());
    persist(path, &journal)?;
    Err(detail)
}

fn persist(path: &Path, journal: &DeliveryJournal) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)
        .and_then(|_| fs::rename(&temporary, path))
        .map_err(|error| format!("cannot persist delivery journal: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Mutex;

    struct FakeRunner {
        calls: Mutex<Vec<Vec<String>>>,
        fail_smoke: bool,
        staged: &'static str,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _directory: &Path, argv: &[String]) -> Result<Output, String> {
            self.calls.lock().unwrap().push(argv.to_vec());
            let is_smoke = argv.first().is_some_and(|value| value == "smoke");
            let is_view = argv.get(2).is_some_and(|value| value == "view");
            let is_staged = argv.get(1).is_some_and(|value| value == "diff");
            Ok(Output {
                status: std::process::ExitStatus::from_raw(if self.fail_smoke && is_smoke {
                    1 << 8
                } else {
                    0
                }),
                stdout: if is_view {
                    b"42\n".to_vec()
                } else if is_staged {
                    self.staged.as_bytes().to_vec()
                } else {
                    Vec::new()
                },
                stderr: if self.fail_smoke && is_smoke {
                    b"unhealthy".to_vec()
                } else {
                    Vec::new()
                },
            })
        }
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, DeliveryConfig) {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        fs::create_dir(&worktree).unwrap();
        let ownership_path = temp.path().join("attempt.ownership.json");
        fs::write(
            &ownership_path,
            serde_json::to_vec(&WorktreeOwnership {
                session_id: "session".into(),
                prd_id: "PRD-1".into(),
                worktree,
                created_at: "now".into(),
                heartbeat_at: "now".into(),
                state: "ready_for_delivery".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let policy = DeliveryConfig {
            enabled: true,
            max_deliveries_per_session: 1,
            command_timeout_ms: 1_000,
            remote: "origin".into(),
            base: "main".into(),
            auto_merge: true,
            staging_environment: "staging".into(),
            deploy_argv: vec!["deploy".into()],
            smoke_argv: vec!["smoke".into()],
            rollback_argv: vec!["rollback".into()],
            comment_blockers: true,
        };
        (temp, ownership_path, policy)
    }

    #[test]
    fn clean_policy_runs_checks_before_merge_and_staging() {
        let (_temp, ownership, policy) = fixture();
        let runner = FakeRunner {
            calls: Mutex::new(Vec::new()),
            fail_smoke: false,
            staged: "src/lib.rs\n",
        };
        let result = deliver_with(&ownership, &policy, &runner).unwrap();
        assert_eq!(result.phase, "staging_verified");
        let calls = runner.calls.lock().unwrap();
        let checks = calls
            .iter()
            .position(|call| call.contains(&"checks".into()))
            .unwrap();
        let merge = calls
            .iter()
            .position(|call| call.contains(&"merge".into()))
            .unwrap();
        let deploy = calls.iter().position(|call| call[0] == "deploy").unwrap();
        assert!(checks < merge && merge < deploy);
    }

    #[test]
    fn failed_smoke_rolls_back_and_comments_blocker() {
        let (_temp, ownership, policy) = fixture();
        let runner = FakeRunner {
            calls: Mutex::new(Vec::new()),
            fail_smoke: true,
            staged: "src/lib.rs\n",
        };
        assert!(deliver_with(&ownership, &policy, &runner).is_err());
        let calls = runner.calls.lock().unwrap();
        assert!(calls.iter().any(|call| call[0] == "rollback"));
        assert!(calls.iter().any(|call| call.contains(&"comment".into())));
        let journal: DeliveryJournal =
            serde_json::from_slice(&fs::read(ownership.with_extension("delivery.json")).unwrap())
                .unwrap();
        assert_eq!(journal.phase, "staging_rolled_back");
    }

    #[test]
    fn production_commands_are_rejected_before_side_effects() {
        let (_temp, ownership, mut policy) = fixture();
        policy.deploy_argv = vec!["deploy".into(), "production".into()];
        let runner = FakeRunner {
            calls: Mutex::new(Vec::new()),
            fail_smoke: false,
            staged: "src/lib.rs\n",
        };
        assert!(deliver_with(&ownership, &policy, &runner)
            .unwrap_err()
            .contains("production"));
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn migration_batches_stop_before_commit_or_publication() {
        let (_temp, ownership, policy) = fixture();
        let runner = FakeRunner {
            calls: Mutex::new(Vec::new()),
            fail_smoke: false,
            staged: "internal/store/migrations/001.sql\n",
        };
        assert!(deliver_with(&ownership, &policy, &runner)
            .unwrap_err()
            .contains("no automatic database rollback"));
        let calls = runner.calls.lock().unwrap();
        assert!(!calls.iter().any(|call| call.contains(&"commit".into())));
        assert!(!calls.iter().any(|call| call.contains(&"push".into())));
    }

    #[test]
    fn process_runner_kills_a_delivery_command_at_its_finite_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let runner = ProcessRunner::new(20);
        let error = runner
            .run(temp.path(), &["/bin/sleep".into(), "5".into()])
            .unwrap_err();
        assert!(error.contains("exceeded delivery command timeout"));
    }
}
