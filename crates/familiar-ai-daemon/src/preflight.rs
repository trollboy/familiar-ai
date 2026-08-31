//! Deterministic, side-effect-free prerequisite checks for unattended work.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::{Command, Stdio};

use familiar_ai_core::{Config, PreflightCommandConfig};

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
}

pub fn run(agents: &AgentSet<'_>, config: &Config, repository: &Path) -> PreflightReport {
    let mut checks = Vec::new();
    if let Some(registry) = &config.worker_registry {
        match crate::run::resolved_worker_plan(config, &crate::run::RouteContext::default()) {
            Ok((_, _, records)) => {
                for record in records {
                    let worker = &registry.workers[&record.selected_worker];
                    let agent = crate::run::build_agent(&worker.as_agent_entry());
                    checks.push(agent_check(
                        &format!("worker.{:?}", record.stage).to_ascii_lowercase(),
                        agent.as_ref(),
                    ));
                }
            }
            Err(detail) => checks.push(PreflightCheck {
                check_id: "worker.routing".into(),
                status: PreflightStatus::Failed,
                detail,
            }),
        }
    } else {
        checks.push(agent_check("agent.implementation", agents.implementation));
        if config.review.enabled {
            checks.push(agent_check("agent.reviewer", agents.reviewer));
        }
    }
    for command in &config.preflight.commands {
        checks.push(command_check(command, repository));
    }
    for verification in config.review.verification.iter().filter(|v| v.required) {
        let directory = repository.join(&verification.working_directory);
        checks.push(writable_path_check(&verification.check_id, &directory));
        checks.push(command_check(
            &PreflightCommandConfig {
                check_id: format!("verification.{}", verification.check_id),
                argv: verification.argv.clone(),
                working_directory: verification.working_directory.clone(),
            },
            repository,
        ));
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
    for (name, provider) in &config.providers {
        if provider.kind == familiar_ai_core::EndpointProviderKind::Inference {
            checks.push(provider_auth_check_with_store(
                name,
                &provider.auth,
                &crate::config_cli::SystemCredentialStore,
            ));
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
    PreflightReport { checks }
}

pub fn provider_auth_check_with_store(
    name: &str,
    auth: &familiar_ai_core::config::AuthDescriptor,
    store: &dyn crate::config_cli::CredentialStore,
) -> PreflightCheck {
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
    let result = config
        .argv
        .split_first()
        .ok_or_else(|| "argv is empty".to_owned())
        .and_then(|(executable, args)| {
            let directory = if config.working_directory.is_empty() {
                repository.to_path_buf()
            } else {
                repository.join(&config.working_directory)
            };
            let output = Command::new(executable)
                .args(args)
                .current_dir(directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|error| format!("cannot launch {executable:?}: {error}"))?;
            if output.success() {
                Ok(())
            } else {
                Err(format!("command exited with code {:?}", output.code()))
            }
        });
    match result {
        Ok(()) => PreflightCheck {
            check_id: config.check_id.clone(),
            status: PreflightStatus::Passed,
            detail: "passed".into(),
        },
        Err(detail) => PreflightCheck {
            check_id: config.check_id.clone(),
            status: PreflightStatus::Failed,
            detail,
        },
    }
}

#[cfg(test)]
mod tests {
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
}
