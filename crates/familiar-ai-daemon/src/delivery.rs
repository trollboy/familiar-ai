//! Explicit, finite delivery boundary for reviewed isolated worktrees.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use familiar_ai_core::{Config, DeliveryConfig, DeliveryMode, EndpointProviderKind};
use familiar_ai_storage::DeliveryRepository;
use rusqlite::Connection;
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
    pub fn new(timeout_ms: u64) -> Self {
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
    if policy.mode == DeliveryMode::Disabled {
        return Err("delivery policy is disabled".into());
    }
    if policy.mode == DeliveryMode::PocSelfApproval {
        let warrant = policy
            .poc_warrant
            .as_ref()
            .ok_or_else(|| "PoC self-approval warrant is missing".to_owned())?;
        let expiry = chrono::DateTime::parse_from_rfc3339(&warrant.expires_at)
            .map_err(|_| "PoC self-approval warrant expiry is invalid".to_owned())?;
        if expiry <= chrono::Utc::now() {
            return Err("PoC self-approval warrant has expired".into());
        }
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
    let mut journal = if journal_path.exists() {
        serde_json::from_slice(
            &fs::read(&journal_path)
                .map_err(|error| format!("cannot resume delivery journal: {error}"))?,
        )
        .map_err(|error| format!("invalid delivery journal: {error}"))?
    } else {
        DeliveryJournal {
            session_id: ownership.session_id,
            prd_id: ownership.prd_id,
            worktree: ownership.worktree,
            branch: branch.clone(),
            pr_number: None,
            phase: "admitted".into(),
            detail: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    };
    persist(&journal_path, &journal)?;

    if phase_before(&journal.phase, "committed") {
        checked(runner, &journal.worktree, &["git", "switch", "-C", &branch])?;
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
            if policy.migration_gate_argv.is_empty() {
                return fail_journal(
                &journal_path,
                journal,
                "deployment_blocked",
                format!("migration change {path} has no automatic database rollback and no configured migration gate; human staging authority required"),
            );
            }
            checked_owned(runner, &journal.worktree, &policy.migration_gate_argv)?;
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
    }
    if phase_before(&journal.phase, "pushed") {
        checked(
            runner,
            &journal.worktree,
            &["git", "push", "-u", &policy.remote, &branch],
        )?;
        journal.phase = "pushed".into();
        persist(&journal_path, &journal)?;
    }

    if phase_before(&journal.phase, "published") {
        let create = provider_argv(
            policy,
            &[
                "pr",
                "create",
                "--fill",
                "--base",
                &policy.base,
                "--head",
                &branch,
            ],
        );
        if let Err(error) = checked_owned(runner, &journal.worktree, &create) {
            journal.detail = Some(format!(
                "PR create returned: {error}; checking for existing PR"
            ));
        }
        let view = checked_owned(
            runner,
            &journal.worktree,
            &provider_argv(
                policy,
                &["pr", "view", &branch, "--json", "number", "--jq", ".number"],
            ),
        )?;
        journal.pr_number = String::from_utf8_lossy(&view.stdout).trim().parse().ok();
        journal.phase = "published".into();
        persist(&journal_path, &journal)?;
    }
    let pr = journal
        .pr_number
        .ok_or_else(|| "provider adapter did not return a pull request number".to_owned())?;

    if policy.mode == DeliveryMode::ReviewedPrManual {
        journal.phase = "awaiting_merge_authority".into();
        persist(&journal_path, &journal)?;
        return Ok(journal);
    }
    if phase_before(&journal.phase, "merged") {
        if let Err(error) = checked_owned(
            runner,
            &journal.worktree,
            &provider_argv(
                policy,
                &["pr", "checks", &pr.to_string(), "--watch", "--fail-fast"],
            ),
        ) {
            comment_blocker(runner, policy, &journal.worktree, pr, &error);
            return fail_journal(&journal_path, journal, "checks_failed", error);
        }
        for check in &policy.required_checks {
            checked_owned(
                runner,
                &journal.worktree,
                &provider_argv(policy, &["pr", "check", &pr.to_string(), check]),
            )?;
        }
    }
    if phase_before(&journal.phase, "merged") {
        checked_owned(
            runner,
            &journal.worktree,
            &provider_argv(
                policy,
                &["pr", "merge", &pr.to_string(), "--merge", "--delete-branch"],
            ),
        )?;
        journal.phase = "merged".into();
        persist(&journal_path, &journal)?;
    }

    if phase_before(&journal.phase, "staging_deployed") {
        if let Err(deploy_error) = checked_owned(runner, &journal.worktree, &policy.deploy_argv) {
            let rollback = checked_owned(runner, &journal.worktree, &policy.rollback_argv)
                .map(|_| "rollback passed".to_owned())
                .unwrap_or_else(|error| format!("rollback failed: {error}"));
            let detail = format!("staging deploy failed: {deploy_error}; {rollback}");
            comment_blocker(runner, policy, &journal.worktree, pr, &detail);
            return fail_journal(&journal_path, journal, "staging_rolled_back", detail);
        }
    }
    if phase_before(&journal.phase, "staging_deployed") {
        journal.phase = "staging_deployed".into();
        persist(&journal_path, &journal)?;
    }
    if phase_before(&journal.phase, "staging_verified") {
        if let Err(smoke_error) = checked_owned(runner, &journal.worktree, &policy.smoke_argv) {
            let rollback = checked_owned(runner, &journal.worktree, &policy.rollback_argv)
                .map(|_| "rollback passed".to_owned())
                .unwrap_or_else(|error| format!("rollback failed: {error}"));
            let detail = format!("staging smoke failed: {smoke_error}; {rollback}");
            comment_blocker(runner, policy, &journal.worktree, pr, &detail);
            return fail_journal(&journal_path, journal, "staging_rolled_back", detail);
        }
    }
    journal.phase = "staging_verified".into();
    journal.detail = None;
    persist(&journal_path, &journal)?;
    Ok(journal)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDeliveryResult {
    pub role: String,
    pub target: String,
    pub revision: String,
    pub smoke_passed: bool,
}

/// Execute one repository-bound deploy target. SSH uses only the ambient
/// agent and OpenSSH configuration: no identity file or credential value is
/// accepted by this boundary.
#[allow(clippy::too_many_arguments)]
pub fn deliver_to_with(
    config: &Config,
    policy: &DeliveryConfig,
    role: &str,
    repository_key: &str,
    session_id: &str,
    prd_id: &str,
    revision: &str,
    external_gates: &[String],
    conn: &Connection,
    directory: &Path,
    runner: &dyn CommandRunner,
) -> Result<TargetDeliveryResult, String> {
    let target_name = policy
        .targets
        .get(role)
        .ok_or_else(|| format!("delivery role '{role}' has no bound deploy target"))?;
    let target = config.providers.get(target_name).ok_or_else(|| {
        format!("delivery role '{role}' references unknown provider '{target_name}'")
    })?;
    if target.kind != EndpointProviderKind::DeployTarget {
        return Err(format!("provider '{target_name}' is not a deploy-target"));
    }
    if role.eq_ignore_ascii_case("production") || role.eq_ignore_ascii_case("prod") {
        if policy.mode != DeliveryMode::ReviewedPrManual {
            return Err("production target requires manual authority; it is unreachable from PoC or automatic mode".into());
        }
    } else if policy.mode == DeliveryMode::ReviewedPrManual {
        return Err("default reviewed-PR mode stops before target delivery; explicit PoC or automatic authority is required".into());
    }
    if policy.mode == DeliveryMode::PocSelfApproval {
        let warrant = policy
            .poc_warrant
            .as_ref()
            .ok_or("PoC self-approval warrant is missing")?;
        let expiry = chrono::DateTime::parse_from_rfc3339(&warrant.expires_at)
            .map_err(|_| "PoC self-approval warrant expiry is invalid")?;
        if expiry <= chrono::Utc::now() {
            return Err("PoC self-approval warrant has expired".into());
        }
    }
    let repo = DeliveryRepository::new(conn);
    for gate in external_gates {
        let evidence = repo
            .resolve_internal_gate(repository_key, gate)
            .map_err(|e| e.to_string())?;
        if !evidence.is_some_and(|e| e.passed) {
            return Err(format!(
                "internal external_gate '{gate}' is unresolved or failing"
            ));
        }
    }
    let recipe = target
        .recipe
        .as_ref()
        .ok_or("deploy-target recipe is missing")?;
    let preflight = vec![
        "ssh".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        target.host.clone(),
        "true".into(),
    ];
    run_effect(
        &repo,
        runner,
        directory,
        repository_key,
        session_id,
        prd_id,
        role,
        target_name,
        revision,
        "ssh_preflight",
        &preflight,
        false,
    )?;
    for (kind, remote) in [
        ("sync", &recipe.sync_argv),
        ("restart", &recipe.restart_argv),
    ] {
        let argv = ssh_argv(&target.host, remote);
        run_effect(
            &repo,
            runner,
            directory,
            repository_key,
            session_id,
            prd_id,
            role,
            target_name,
            revision,
            kind,
            &argv,
            false,
        )?;
    }
    let smoke = ssh_argv(&target.host, &recipe.smoke_argv);
    run_effect(
        &repo,
        runner,
        directory,
        repository_key,
        session_id,
        prd_id,
        role,
        target_name,
        revision,
        "smoke",
        &smoke,
        true,
    )?;
    Ok(TargetDeliveryResult {
        role: role.into(),
        target: target_name.clone(),
        revision: revision.into(),
        smoke_passed: true,
    })
}

fn ssh_argv(host: &str, remote: &[String]) -> Vec<String> {
    let command = remote
        .iter()
        .map(|value| shell_quote(value))
        .collect::<Vec<_>>()
        .join(" ");
    vec![
        "ssh".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        host.into(),
        command,
    ]
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[allow(clippy::too_many_arguments)]
fn run_effect(
    repo: &DeliveryRepository<'_>,
    runner: &dyn CommandRunner,
    directory: &Path,
    repository_key: &str,
    session_id: &str,
    prd_id: &str,
    role: &str,
    target: &str,
    revision: &str,
    kind: &str,
    argv: &[String],
    retain_output: bool,
) -> Result<(), String> {
    let key = format!("{session_id}:{prd_id}:{role}:{target}:{revision}:{kind}");
    let effect_id = format!("effect-{}", hex_digest(key.as_bytes()));
    let existing = repo
        .begin_effect(&effect_id, repository_key, session_id, prd_id, kind, &key)
        .map_err(|e| e.to_string())?;
    if existing.status == "succeeded" {
        return Ok(());
    }
    if existing.status == "failed" {
        return Err(existing.detail.unwrap_or_else(|| {
            "delivery previously stopped; operator intervention required".into()
        }));
    }
    let output = runner.run(directory, argv);
    match output {
        Ok(output) => {
            let mut retained = output.stdout.clone();
            retained.extend_from_slice(&output.stderr);
            let detail = if output.status.success() {
                None
            } else {
                Some(familiar_ai_agent::redact_sensitive(format!("{kind} failed; restore SSH agent authentication/reachability, correct the target, then start a new delivery session: {}", String::from_utf8_lossy(&output.stderr).trim())))
            };
            if retain_output {
                repo.finish_evidence(
                    &key,
                    output.status.success(),
                    target,
                    revision,
                    &retained,
                    detail.as_deref(),
                )
                .map_err(|e| e.to_string())?;
                // Role is the stable environment identity used by gate names.
                repo.finish_effect(&key, output.status.success(), Some(role), detail.as_deref())
                    .map_err(|e| e.to_string())?;
            } else {
                repo.finish_effect(&key, output.status.success(), Some(role), detail.as_deref())
                    .map_err(|e| e.to_string())?;
            }
            if output.status.success() {
                Ok(())
            } else {
                Err(detail.unwrap())
            }
        }
        Err(error) => {
            let detail = familiar_ai_agent::redact_sensitive(format!("{kind} could not run; restore SSH agent authentication/reachability, verify ~/.ssh/config, then start a new delivery session: {error}"));
            repo.finish_effect(&key, false, Some(role), Some(&detail))
                .map_err(|e| e.to_string())?;
            Err(detail)
        }
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    use ring::digest::{digest, SHA256};
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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

fn provider_argv(policy: &DeliveryConfig, values: &[&str]) -> Vec<String> {
    policy
        .provider_argv
        .iter()
        .cloned()
        .chain(values.iter().map(|v| (*v).to_owned()))
        .collect()
}

fn phase_before(current: &str, target: &str) -> bool {
    fn rank(value: &str) -> u8 {
        match value {
            "admitted" => 0,
            "committed" => 1,
            "pushed" => 2,
            "published" | "awaiting_merge_authority" => 3,
            "merged" => 4,
            "checks_failed" => 3,
            "staging_rolled_back" => 4,
            "staging_deployed" => 5,
            "staging_verified" => 6,
            _ => 0,
        }
    }
    rank(current) < rank(target)
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
        Err(familiar_ai_agent::redact_sensitive(format!(
            "{:?} exited {:?}: {}",
            values,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
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
        let detail = familiar_ai_agent::redact_sensitive(detail.to_owned());
        let _ = checked_owned(
            runner,
            directory,
            &provider_argv(
                policy,
                &["pr", "comment", &pr.to_string(), "--body", &detail],
            ),
        );
    }
}

fn fail_journal(
    path: &Path,
    mut journal: DeliveryJournal,
    phase: &str,
    detail: String,
) -> Result<DeliveryJournal, String> {
    let detail = familiar_ai_agent::redact_sensitive(detail);
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
    use familiar_ai_core::config::{AuthDescriptor, ProviderConfig};
    use familiar_ai_core::{DeployRecipeConfig, EndpointProviderKind};
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Mutex;

    struct FakeRunner {
        calls: Mutex<Vec<Vec<String>>>,
        fail_deploy: bool,
        fail_smoke: bool,
        staged: &'static str,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _directory: &Path, argv: &[String]) -> Result<Output, String> {
            self.calls.lock().unwrap().push(argv.to_vec());
            let is_smoke = argv.first().is_some_and(|value| value == "smoke");
            let is_deploy = argv.first().is_some_and(|value| value == "deploy");
            let is_view = argv.get(2).is_some_and(|value| value == "view");
            let is_staged = argv.get(1).is_some_and(|value| value == "diff");
            Ok(Output {
                status: std::process::ExitStatus::from_raw(
                    if (self.fail_smoke && is_smoke) || (self.fail_deploy && is_deploy) {
                        1 << 8
                    } else {
                        0
                    },
                ),
                stdout: if is_view {
                    b"42\n".to_vec()
                } else if is_staged {
                    self.staged.as_bytes().to_vec()
                } else {
                    Vec::new()
                },
                stderr: if self.fail_smoke && is_smoke {
                    b"unhealthy".to_vec()
                } else if self.fail_deploy && is_deploy {
                    b"partial deploy".to_vec()
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
            mode: DeliveryMode::ReviewGatedAutomatic,
            enabled: true,
            max_deliveries_per_session: 1,
            command_timeout_ms: 1_000,
            remote: "origin".into(),
            base: "main".into(),
            auto_merge: true,
            provider_argv: vec!["gh".into()],
            staging_environment: "staging".into(),
            deploy_argv: vec!["deploy".into()],
            smoke_argv: vec!["smoke".into()],
            rollback_argv: vec!["rollback".into()],
            comment_blockers: true,
            review_gate: Some(familiar_ai_core::ReviewGateConfig {
                implementer: "impl".into(),
                reviewer: "review".into(),
                approver: "approve".into(),
            }),
            ..DeliveryConfig::default()
        };
        (temp, ownership_path, policy)
    }

    #[test]
    fn clean_policy_runs_checks_before_merge_and_staging() {
        let (_temp, ownership, policy) = fixture();
        let runner = FakeRunner {
            calls: Mutex::new(Vec::new()),
            fail_deploy: false,
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
            fail_deploy: false,
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
    fn failed_deploy_rolls_back_and_comments_blocker() {
        let (_temp, ownership, policy) = fixture();
        let runner = FakeRunner {
            calls: Mutex::new(Vec::new()),
            fail_deploy: true,
            fail_smoke: false,
            staged: "src/lib.rs\n",
        };
        let error = deliver_with(&ownership, &policy, &runner).unwrap_err();
        assert!(error.contains("staging deploy failed"));
        assert!(error.contains("rollback passed"));
        let calls = runner.calls.lock().unwrap();
        assert!(calls.iter().any(|call| call[0] == "rollback"));
        assert!(calls.iter().any(|call| call.contains(&"comment".into())));
    }

    #[test]
    fn retry_after_partial_deploy_does_not_repeat_publish_or_merge() {
        let (_temp, ownership, policy) = fixture();
        let runner = FakeRunner {
            calls: Mutex::new(Vec::new()),
            fail_deploy: true,
            fail_smoke: false,
            staged: "src/lib.rs\n",
        };
        assert!(deliver_with(&ownership, &policy, &runner).is_err());
        assert!(deliver_with(&ownership, &policy, &runner).is_err());
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.contains(&"merge".into()))
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.get(1).is_some_and(|v| v == "push"))
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.first().is_some_and(|v| v == "deploy"))
                .count(),
            2
        );
    }

    #[test]
    fn manual_policy_publishes_pr_and_stops_before_checks_merge_or_deploy() {
        let (_temp, ownership, mut policy) = fixture();
        policy.mode = DeliveryMode::ReviewedPrManual;
        let runner = FakeRunner {
            calls: Mutex::new(Vec::new()),
            fail_deploy: false,
            fail_smoke: false,
            staged: "src/lib.rs\n",
        };
        let result = deliver_with(&ownership, &policy, &runner).unwrap();
        assert_eq!(result.phase, "awaiting_merge_authority");
        let calls = runner.calls.lock().unwrap();
        assert!(!calls.iter().any(|call| call.contains(&"merge".into())));
        assert!(!calls
            .iter()
            .any(|call| call.first().is_some_and(|v| v == "deploy")));
    }

    #[test]
    fn production_commands_are_rejected_before_side_effects() {
        let (_temp, ownership, mut policy) = fixture();
        policy.deploy_argv = vec!["deploy".into(), "production".into()];
        let runner = FakeRunner {
            calls: Mutex::new(Vec::new()),
            fail_deploy: false,
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
            fail_deploy: false,
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

    fn target_fixture(
        mode: DeliveryMode,
    ) -> (Config, DeliveryConfig, familiar_ai_storage::Database) {
        let mut config = Config::default();
        config.providers.insert(
            "box".into(),
            ProviderConfig {
                kind: EndpointProviderKind::DeployTarget,
                runtime: None,
                host: "box.example".into(),
                auth: AuthDescriptor::SshAgent,
                models: vec![],
                verified_at: Some("2026-01-01T00:00:00Z".into()),
                capabilities: vec!["linux".into()],
                recipe: Some(DeployRecipeConfig {
                    sync_argv: vec!["sync".into()],
                    restart_argv: vec!["restart".into()],
                    smoke_argv: vec!["smoke".into()],
                }),
            },
        );
        let mut policy = DeliveryConfig {
            mode,
            ..DeliveryConfig::default()
        };
        policy.targets.insert("staging".into(), "box".into());
        let db = familiar_ai_storage::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        (config, policy, db)
    }

    #[test]
    fn target_recipe_records_all_effects_and_retains_smoke_identity_and_output() {
        let (config, policy, db) = target_fixture(DeliveryMode::ReviewGatedAutomatic);
        let runner = FakeRunner {
            calls: Mutex::new(vec![]),
            fail_deploy: false,
            fail_smoke: false,
            staged: "",
        };
        let result = deliver_to_with(
            &config,
            &policy,
            "staging",
            "repo",
            "session",
            "PRD-48",
            "abc123",
            &[],
            db.conn(),
            Path::new("."),
            &runner,
        )
        .unwrap();
        assert!(result.smoke_passed);
        assert_eq!(runner.calls.lock().unwrap().len(), 4);
        let rows: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM delivery_external_effects WHERE status='succeeded'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 4);
        let (target, revision, output): (String, String, Vec<u8>) = db.conn().query_row("SELECT target,revision,output FROM delivery_external_effects WHERE effect_kind='smoke'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!((target.as_str(), revision.as_str()), ("box", "abc123"));
        assert_eq!(output, Vec::<u8>::new());
        let evidence = familiar_ai_storage::DeliveryRepository::new(db.conn())
            .resolve_internal_gate("repo", "deploy:staging-smoke-passing")
            .unwrap()
            .unwrap();
        assert!(evidence.passed);
    }

    #[test]
    fn production_is_unreachable_from_poc_before_any_external_effect() {
        let (config, mut policy, db) = target_fixture(DeliveryMode::PocSelfApproval);
        policy.targets.insert("production".into(), "box".into());
        let runner = FakeRunner {
            calls: Mutex::new(vec![]),
            fail_deploy: false,
            fail_smoke: false,
            staged: "",
        };
        let error = deliver_to_with(
            &config,
            &policy,
            "production",
            "repo",
            "session",
            "PRD-48",
            "abc",
            &[],
            db.conn(),
            Path::new("."),
            &runner,
        )
        .unwrap_err();
        assert!(error.contains("manual authority"));
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn unresolved_internal_gate_blocks_before_effects() {
        let (config, policy, db) = target_fixture(DeliveryMode::ReviewGatedAutomatic);
        let runner = FakeRunner {
            calls: Mutex::new(vec![]),
            fail_deploy: false,
            fail_smoke: false,
            staged: "",
        };
        assert!(deliver_to_with(
            &config,
            &policy,
            "staging",
            "repo",
            "session",
            "PRD-48",
            "abc",
            &["verification:missing".into()],
            db.conn(),
            Path::new("."),
            &runner
        )
        .unwrap_err()
        .contains("unresolved or failing"));
        assert!(runner.calls.lock().unwrap().is_empty());
    }
}
