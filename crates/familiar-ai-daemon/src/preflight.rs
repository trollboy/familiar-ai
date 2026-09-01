//! Deterministic, side-effect-free prerequisite checks for unattended work.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use familiar_ai_core::{Config, PreflightCommandConfig};
use familiar_ai_review::contains_secret;

use crate::run::AgentSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightCheck {
    pub check_id: String,
    pub status: PreflightStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightStatus {
    Passed,
    Failed,
    EnvironmentDenied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    pub checks: Vec<PreflightCheck>,
}

impl PreflightReport {
    pub fn is_valid(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.status == PreflightStatus::Passed)
    }

    pub fn failure_summary(&self) -> String {
        self.checks
            .iter()
            .filter(|check| check.status != PreflightStatus::Passed)
            .map(|check| format!("{}: {}", check.check_id, check.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn session_summary(&self) -> String {
        self.checks
            .iter()
            .map(|check| {
                format!(
                    "{} [{}]: {}",
                    check.check_id,
                    match check.status {
                        PreflightStatus::Passed => "passed",
                        PreflightStatus::Failed => "failed",
                        PreflightStatus::EnvironmentDenied => "environment_denied",
                    },
                    check.detail
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

pub fn run(agents: &AgentSet<'_>, config: &Config, repository: &Path) -> PreflightReport {
    let mut checks = Vec::new();
    let mut probed_agents = BTreeSet::new();
    if let Some(registry) = &config.worker_registry {
        match crate::run::resolved_worker_plan(config, &crate::run::RouteContext::default()) {
            Ok((_, _, records)) => {
                for record in records {
                    let worker = &registry.workers[&record.selected_worker];
                    let key = format!(
                        "executable:{}:{}:{:?}",
                        worker.runtime_id().unwrap_or("invalid-runtime"),
                        worker.executable.as_deref().unwrap_or("default"),
                        worker.extra_args
                    );
                    let check_id = format!("worker.{:?}", record.stage).to_ascii_lowercase();
                    if probed_agents.insert(key.clone()) {
                        let agent = crate::run::build_agent(&worker.as_agent_entry());
                        checks.push(agent_check(&check_id, agent.as_ref()));
                    } else {
                        checks.push(deduplicated_check(&check_id, &key));
                    }
                }
            }
            Err(detail) => checks.push(PreflightCheck {
                check_id: "worker.routing".into(),
                status: PreflightStatus::Failed,
                detail,
            }),
        }
    } else {
        let implementation_key = agent_identity(agents.implementation);
        probed_agents.insert(implementation_key.clone());
        checks.push(agent_check("agent.implementation", agents.implementation));
        if config.review.enabled {
            let reviewer_key = agent_identity(agents.reviewer);
            if probed_agents.insert(reviewer_key.clone()) {
                checks.push(agent_check("agent.reviewer", agents.reviewer));
            } else {
                checks.push(deduplicated_check("agent.reviewer", &reviewer_key));
            }
        }
    }
    for name in &config.preflight.required_environment {
        let present = std::env::var_os(name).is_some_and(|value| !value.is_empty());
        checks.push(PreflightCheck {
            check_id: format!("environment.{name}"),
            status: if present {
                PreflightStatus::Passed
            } else {
                PreflightStatus::Failed
            },
            detail: if present {
                "present (value redacted)".into()
            } else {
                "missing or empty".into()
            },
        });
    }
    // Provider authentication is resolved in the daemon itself. In
    // particular, credential-store references do not depend on a login shell
    // or an environment-variable bridge inherited by the supervisor.
    let routed_providers = config.worker_registry.as_ref().map(|registry| {
        crate::run::resolved_worker_plan(config, &crate::run::RouteContext::default())
            .map(|(_, _, records)| {
                records
                    .into_iter()
                    .map(|record| registry.workers[&record.selected_worker].provider.as_str())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default()
    });
    let mut auth_probes = BTreeSet::new();
    for (name, provider) in &config.providers {
        if provider.kind == familiar_ai_core::EndpointProviderKind::Inference {
            if routed_providers
                .as_ref()
                .is_some_and(|providers| !providers.contains(name.as_str()))
            {
                continue;
            }
            let key = String::from(provider.auth.clone());
            if auth_probes.insert(key.clone()) {
                checks.push(provider_auth_check_with_store(
                    name,
                    &provider.auth,
                    &crate::config_cli::SystemCredentialStore,
                ));
            } else {
                checks.push(deduplicated_check(&format!("provider_auth.{name}"), &key));
            }
        }
    }
    // Expire deploy-target authentication before an unattended claim. Only
    // targets reachable through this repository's role bindings are probed.
    match config.repository(repository) {
        Ok(repository_config) => {
            if let Some(delivery) = &repository_config.delivery {
                for target_name in delivery.targets.values() {
                    match config.providers.get(target_name) {
                        Some(target)
                            if target.kind
                                == familiar_ai_core::EndpointProviderKind::DeployTarget =>
                        {
                            checks.push(command_check(
                                &PreflightCommandConfig {
                                    check_id: format!("deploy_target.{target_name}"),
                                    argv: vec![
                                        "ssh".into(),
                                        "-o".into(),
                                        "BatchMode=yes".into(),
                                        "-o".into(),
                                        "ConnectTimeout=10".into(),
                                        target.host.clone(),
                                        "true".into(),
                                    ],
                                    working_directory: String::new(),
                                },
                                repository,
                            ));
                        }
                        _ => checks.push(PreflightCheck {
                            check_id: format!("deploy_target.{target_name}"),
                            status: PreflightStatus::Failed,
                            detail: "role binding does not resolve to a deploy-target provider"
                                .into(),
                        }),
                    }
                }
            }
        }
        Err(detail) => checks.push(PreflightCheck {
            check_id: "repository.delivery_targets".into(),
            status: PreflightStatus::Failed,
            detail: detail.to_string(),
        }),
    }
    // Explicit prerequisite commands and potentially costly verification run
    // only after all declared authentication/network targets above have been
    // proven reachable.
    let mut command_probes = BTreeSet::new();
    for command in &config.preflight.commands {
        let key = format!("{:?}|{}", command.argv, command.working_directory);
        if command_probes.insert(key.clone()) {
            checks.push(command_check(command, repository));
        } else {
            checks.push(deduplicated_check(&command.check_id, &key));
        }
    }
    for verification in config.review.verification.iter().filter(|v| v.required) {
        let directory = repository.join(&verification.working_directory);
        checks.push(writable_path_check(&verification.check_id, &directory));
        let key = format!(
            "{:?}|{}|{:?}|{}",
            verification.argv,
            verification.working_directory,
            verification.environment,
            verification.timeout_ms
        );
        let check_id = format!("verification.{}", verification.check_id);
        if command_probes.insert(key.clone()) {
            checks.push(verification_check(verification, repository));
        } else {
            checks.push(deduplicated_check(&check_id, &key));
        }
    }
    PreflightReport { checks }
}

fn agent_identity(agent: &dyn familiar_ai_agent::CodingAgent) -> String {
    format!(
        "in-process:{:x}",
        (agent as *const dyn familiar_ai_agent::CodingAgent as *const ()) as usize
    )
}

fn deduplicated_check(check_id: &str, identity: &str) -> PreflightCheck {
    PreflightCheck {
        check_id: check_id.into(),
        status: PreflightStatus::Passed,
        detail: format!("deduplicated: reused session probe {identity}"),
    }
}

pub fn provider_auth_check_with_store(
    name: &str,
    auth: &familiar_ai_core::config::AuthDescriptor,
    store: &dyn crate::config_cli::CredentialStore,
) -> PreflightCheck {
    let _heartbeat = PhaseHeartbeat::start(format!("provider_auth.{name}"), "daemon".into());
    let descriptor = String::from(auth.clone());
    match crate::config_cli::check_auth_with_store(auth, store) {
        Ok(_) => PreflightCheck {
            check_id: format!("provider_auth.{name}"),
            status: PreflightStatus::Passed,
            detail: format!("available via {descriptor}"),
        },
        Err(condition) => PreflightCheck {
            check_id: format!("provider_auth.{name}"),
            status: PreflightStatus::Failed,
            detail: condition,
        },
    }
}

fn writable_path_check(check_id: &str, path: &Path) -> PreflightCheck {
    let _heartbeat = PhaseHeartbeat::start(format!("writable.{check_id}"), "daemon".into());
    let probe = path.join(format!(".verification-write-probe-{}", std::process::id()));
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|_| fs::remove_file(&probe));
    match result {
        Ok(()) => PreflightCheck {
            check_id: format!("writable.{check_id}"),
            status: PreflightStatus::Passed,
            detail: format!("writable: {}", path.display()),
        },
        Err(error) => PreflightCheck {
            check_id: format!("writable.{check_id}"),
            status: PreflightStatus::EnvironmentDenied,
            detail: format!(
                "environment denied writable path {}: {error}",
                path.display()
            ),
        },
    }
}

fn agent_check(check_id: &str, agent: &dyn familiar_ai_agent::CodingAgent) -> PreflightCheck {
    let _heartbeat = PhaseHeartbeat::start(check_id.into(), "in-process-agent".into());
    match agent.preflight() {
        Ok(()) => PreflightCheck {
            check_id: check_id.into(),
            status: PreflightStatus::Passed,
            detail: "available".into(),
        },
        Err(detail) => PreflightCheck {
            check_id: check_id.into(),
            status: PreflightStatus::Failed,
            detail,
        },
    }
}

fn command_check(config: &PreflightCommandConfig, repository: &Path) -> PreflightCheck {
    let directory = if config.working_directory.is_empty() {
        repository.to_path_buf()
    } else {
        repository.join(&config.working_directory)
    };
    execute_command(
        &config.check_id,
        &config.argv,
        &directory,
        None,
        Duration::from_secs(300),
    )
}

fn verification_check(
    config: &familiar_ai_core::config::ReviewVerificationConfig,
    repository: &Path,
) -> PreflightCheck {
    execute_command(
        &format!("verification.{}", config.check_id),
        &config.argv,
        &repository.join(&config.working_directory),
        Some(&config.environment),
        Duration::from_millis(config.timeout_ms),
    )
}

const MAX_RETAINED_OUTPUT: usize = 16 * 1024;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

fn execute_command(
    check_id: &str,
    argv: &[String],
    directory: &Path,
    environment: Option<&BTreeMap<String, String>>,
    timeout: Duration,
) -> PreflightCheck {
    let Some((executable, args)) = argv.split_first() else {
        return PreflightCheck {
            check_id: check_id.into(),
            status: PreflightStatus::Failed,
            detail: "argv is empty".into(),
        };
    };
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(environment) = environment {
        command.env_clear().envs(environment);
    }
    let started = Instant::now();
    preflight_phase_heartbeat(check_id, started.elapsed(), "pending-spawn");
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let denied = matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
            );
            return PreflightCheck {
                check_id: check_id.into(),
                status: if denied {
                    PreflightStatus::EnvironmentDenied
                } else {
                    PreflightStatus::Failed
                },
                detail: if denied {
                    format!("environment denied launching {executable:?}: {error}")
                } else {
                    format!("cannot launch {executable:?}: {error}")
                },
            };
        }
    };
    let child_id = child.id();
    preflight_heartbeat(check_id, started.elapsed(), child_id);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let last_line = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let stdout_line = std::sync::Arc::clone(&last_line);
    let stderr_line = std::sync::Arc::clone(&last_line);
    let stdout_thread = thread::spawn(move || read_bounded(stdout, &stdout_line));
    let stderr_thread = thread::spawn(move || read_bounded(stderr, &stderr_line));
    let mut next_heartbeat = HEARTBEAT_INTERVAL;
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Ok(status), false),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                break (child.wait(), true);
            }
            Ok(None) => {}
            Err(error) => break (Err(error), false),
        }
        if started.elapsed() >= next_heartbeat {
            let recent = last_line
                .lock()
                .map(|line| line.clone())
                .unwrap_or_default();
            preflight_heartbeat_with_activity(check_id, started.elapsed(), child_id, &recent);
            next_heartbeat = next_heartbeat.saturating_add(HEARTBEAT_INTERVAL);
        }
        thread::sleep(Duration::from_millis(5));
    };
    preflight_heartbeat(check_id, started.elapsed(), child_id);
    let (stdout, stdout_omitted) = stdout_thread.join().unwrap_or_default();
    let (stderr, stderr_omitted) = stderr_thread.join().unwrap_or_default();
    let output = retained_output(
        &stdout,
        &stderr,
        stdout_omitted + stderr_omitted,
        environment,
    );
    let denied = output_indicates_environment_denial(&stdout, &stderr);
    match status {
        Ok(status) if status.success() && !timed_out => PreflightCheck {
            check_id: check_id.into(),
            status: PreflightStatus::Passed,
            detail: if output.is_empty() {
                "passed".into()
            } else {
                format!("passed; {output}")
            },
        },
        Ok(status) => PreflightCheck {
            check_id: check_id.into(),
            status: if denied {
                PreflightStatus::EnvironmentDenied
            } else {
                PreflightStatus::Failed
            },
            detail: if timed_out {
                format!("timed out after {} ms; {output}", timeout.as_millis())
            } else if denied {
                format!(
                    "environment denied check {check_id} (exit {:?}); {output}",
                    status.code()
                )
            } else {
                format!("command exited with code {:?}; {output}", status.code())
            },
        },
        Err(error) => PreflightCheck {
            check_id: check_id.into(),
            status: PreflightStatus::Failed,
            detail: format!("cannot wait for child {child_id}: {error}; {output}"),
        },
    }
}

fn output_indicates_environment_denial(stdout: &[u8], stderr: &[u8]) -> bool {
    let mut output = String::from_utf8_lossy(stdout).to_ascii_lowercase();
    output.push_str(&String::from_utf8_lossy(stderr).to_ascii_lowercase());
    [
        "operation not permitted",
        "permission denied",
        "network is unreachable",
        "cannot assign requested address",
        "read-only file system",
    ]
    .iter()
    .any(|marker| output.contains(marker))
}

fn read_bounded<R: Read>(
    reader: Option<R>,
    last_line: &std::sync::Arc<std::sync::Mutex<String>>,
) -> (Vec<u8>, usize) {
    let Some(mut reader) = reader else {
        return (Vec::new(), 0);
    };
    let mut retained = Vec::new();
    let mut omitted = 0usize;
    let mut buffer = [0u8; 4096];
    let mut current = Vec::new();
    while let Ok(count) = reader.read(&mut buffer) {
        if count == 0 {
            break;
        }
        // Track the most recent complete line so heartbeats can say what the
        // child is doing, not merely that it is alive (the lock-collision
        // diagnosis took twenty minutes that this line answers at thirty
        // seconds).
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                if !current.is_empty() {
                    if let Ok(mut guard) = last_line.lock() {
                        *guard = String::from_utf8_lossy(&current).into_owned();
                    }
                }
                current.clear();
            } else if current.len() < 200 {
                current.push(*byte);
            }
        }
        let available = MAX_RETAINED_OUTPUT.saturating_sub(retained.len());
        let keep = count.min(available);
        retained.extend_from_slice(&buffer[..keep]);
        omitted = omitted.saturating_add(count - keep);
    }
    (retained, omitted)
}

fn retained_output(
    stdout: &[u8],
    stderr: &[u8],
    omitted: usize,
    environment: Option<&BTreeMap<String, String>>,
) -> String {
    // FAM-BUG-028: redact per LINE, never the whole capture — one
    // credential-shaped string (this repo's own auth-test fixtures print
    // them) must not erase the failing test's name from the evidence.
    let redact = |bytes: &[u8]| -> String {
        String::from_utf8_lossy(bytes)
            .trim()
            .lines()
            .map(|line| {
                let secret = contains_secret(line.as_bytes())
                    || environment.is_some_and(|values| {
                        values
                            .values()
                            .any(|value| !value.is_empty() && line.contains(value.as_str()))
                    });
                if secret {
                    "[REDACTED LINE]"
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\\n")
    };
    let stdout = redact(stdout);
    let stderr = redact(stderr);
    format!("stdout={stdout:?} stderr={stderr:?} omitted_bytes={omitted}")
}

fn preflight_heartbeat(check_id: &str, elapsed: Duration, child_id: u32) {
    preflight_phase_heartbeat(check_id, elapsed, &format!("pid:{child_id}"));
}

/// Heartbeat carrying the child's most recent output line, redacted per the
/// same rules as retained evidence and truncated for one-line legibility.
fn preflight_heartbeat_with_activity(
    check_id: &str,
    elapsed: Duration,
    child_id: u32,
    recent: &str,
) {
    if recent.is_empty() {
        preflight_heartbeat(check_id, elapsed, child_id);
        return;
    }
    let shown = if contains_secret(recent.as_bytes()) {
        "[REDACTED LINE]".to_owned()
    } else {
        let mut line: String = recent.chars().take(120).collect();
        if line.len() < recent.len() {
            line.push('…');
        }
        line
    };
    eprintln!(
        "preflight: heartbeat check={check_id} elapsed_ms={} child=pid:{child_id} last={shown:?}",
        elapsed.as_millis()
    );
}

fn preflight_phase_heartbeat(check_id: &str, elapsed: Duration, child_identity: &str) {
    eprintln!(
        "preflight: heartbeat check={check_id} elapsed_ms={} child={child_identity}",
        elapsed.as_millis()
    );
    let _ = std::io::stderr().flush();
}

struct PhaseHeartbeat {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl PhaseHeartbeat {
    fn start(check_id: String, child_identity: String) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        preflight_phase_heartbeat(&check_id, Duration::ZERO, &child_identity);
        let thread = thread::spawn(move || {
            let started = Instant::now();
            while !flag.load(std::sync::atomic::Ordering::Acquire) {
                thread::park_timeout(HEARTBEAT_INTERVAL);
                if !flag.load(std::sync::atomic::Ordering::Acquire) {
                    preflight_phase_heartbeat(&check_id, started.elapsed(), &child_identity);
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for PhaseHeartbeat {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn secret_lines_redact_individually_and_failures_stay_named() {
        let stdout = concat!(
            "test billing::collects_costs ... ok\n",
            "authorization: bearer sk-live-abc123def456ghi789\n",
            "test worker_lock::tests::simultaneous_fallback_claims_have_exactly_one_winner ... FAILED\n",
        );
        let detail = super::retained_output(stdout.as_bytes(), b"", 0, None);
        assert!(
            detail.contains("simultaneous_fallback_claims_have_exactly_one_winner ... FAILED"),
            "failing test must stay named: {detail}"
        );
        assert!(detail.contains("[REDACTED LINE]"), "{detail}");
        assert!(!detail.contains("sk-live-abc123def456ghi789"), "{detail}");
    }

    use super::*;
    use familiar_ai_agent::{AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult};

    struct AvailableAgent;
    impl CodingAgent for AvailableAgent {
        fn execute(
            &self,
            _request: ExecutionRequest<'_>,
            _output: &mut dyn std::io::Write,
        ) -> Result<ExecutionResult, AgentExecutionError> {
            panic!("preflight must not execute an agent")
        }
    }

    #[test]
    fn commands_and_missing_credentials_are_reported_without_secret_values() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.preflight.commands = vec![
            PreflightCommandConfig {
                check_id: "available".into(),
                argv: vec!["/usr/bin/true".into()],
                working_directory: String::new(),
            },
            PreflightCommandConfig {
                check_id: "failed".into(),
                argv: vec!["/usr/bin/false".into()],
                working_directory: String::new(),
            },
        ];
        config.preflight.required_environment =
            vec!["FAMILIAR_AI_TEST_DEFINITELY_MISSING_CREDENTIAL".into()];
        let agent = AvailableAgent;
        let report = run(
            &AgentSet {
                implementation: &agent,
                reviewer: &agent,
                remediation: &agent,
            },
            &config,
            temp.path(),
        );
        assert!(!report.is_valid());
        assert!(report.failure_summary().contains("failed"));
        assert!(report.failure_summary().contains("missing or empty"));
        assert!(!report.failure_summary().contains("credential="));
    }

    #[test]
    fn unused_inventory_provider_does_not_block_worker_preflight() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.providers.insert(
            "unused".into(),
            familiar_ai_core::config::ProviderConfig {
                kind: familiar_ai_core::EndpointProviderKind::Inference,
                billing_mode: None,
                organization_id: None,
                organization_name: None,
                project_id: None,
                runtime: None,
                host: "127.0.0.1:1".into(),
                via: None,
                auth: familiar_ai_core::config::AuthDescriptor::Env(
                    "FAMILIAR_AI_TEST_DEFINITELY_MISSING_UNUSED_KEY".into(),
                ),
                models: vec!["unused-model".into()],
                verified_at: None,
                capabilities: Vec::new(),
                recipe: None,
            },
        );
        config.worker_registry = Some(familiar_ai_core::config::WorkerRegistryConfig::default());
        let agent = AvailableAgent;
        let report = run(
            &AgentSet {
                implementation: &agent,
                reviewer: &agent,
                remediation: &agent,
            },
            &config,
            temp.path(),
        );

        assert!(report
            .checks
            .iter()
            .all(|check| check.check_id != "provider_auth.unused"));
    }
}
