//! PRD-078 regressions for faithful, bounded, once-per-session preflight.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

use familiar_ai_agent::{AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult};
use familiar_ai_core::{config::ReviewVerificationConfig, Config};
use familiar_ai_daemon::preflight::{run, PreflightStatus};
use familiar_ai_daemon::run::AgentSet;

struct CountingAgent(AtomicUsize);

impl CodingAgent for CountingAgent {
    fn preflight(&self) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn execute(
        &self,
        _: ExecutionRequest<'_>,
        _: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        unreachable!()
    }
}

fn agents(agent: &CountingAgent) -> AgentSet<'_> {
    AgentSet {
        implementation: agent,
        reviewer: agent,
        remediation: agent,
    }
}

fn verification(
    check_id: &str,
    script: &str,
    environment: BTreeMap<String, String>,
    timeout_ms: u64,
) -> ReviewVerificationConfig {
    ReviewVerificationConfig {
        check_id: check_id.into(),
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        working_directory: ".".into(),
        timeout_ms,
        required: true,
        path_prefixes: Vec::new(),
        environment,
    }
}

#[test]
fn exit_101_retains_the_check_and_bounded_redacted_diagnostics() {
    let repository = tempfile::tempdir().unwrap();
    let agent = CountingAgent(AtomicUsize::new(0));
    let secret = "sessiontoken0123456789";
    let mut config = Config::default();
    config.review.verification = vec![verification(
        "pinned-exit-101",
        // A secret-bearing line AND a diagnostic line: FAM-BUG-028 requires
        // redaction to be per line, never evidence-erasing — the failing
        // detail must survive next to the redacted credential.
        "printf 'failure: %s\\n' \"$TOKEN\" >&2; printf 'test cli_run::example ... FAILED\\n' >&2; exit 101",
        [("TOKEN".into(), secret.into())].into(),
        1_000,
    )];

    let report = run(&agents(&agent), &config, repository.path());
    let summary = report.failure_summary();
    assert!(summary.contains("verification.pinned-exit-101"));
    assert!(summary.contains("code Some(101)"));
    assert!(summary.contains("[REDACTED LINE]"));
    assert!(
        summary.contains("test cli_run::example ... FAILED"),
        "diagnostics must survive beside the redacted line: {summary}"
    );
    assert!(!summary.contains(secret));
    assert!(summary.len() < 20_000);
}

#[test]
fn declared_environment_and_timeout_are_executed_without_conversion() {
    let repository = tempfile::tempdir().unwrap();
    let agent = CountingAgent(AtomicUsize::new(0));
    let mut config = Config::default();
    config.review.verification = vec![verification(
        "exact-spec",
        "test \"$ONLY_DECLARED\" = yes; sleep 1",
        [("ONLY_DECLARED".into(), "yes".into())].into(),
        20,
    )];

    let report = run(&agents(&agent), &config, repository.path());
    let check = report
        .checks
        .iter()
        .find(|check| check.check_id == "verification.exact-spec")
        .unwrap();
    assert_eq!(check.status, PreflightStatus::Failed);
    assert!(check.detail.contains("timed out after 20 ms"));
}

#[test]
fn identical_agent_and_verification_probes_are_reused_and_reported() {
    let repository = tempfile::tempdir().unwrap();
    let counter = repository.path().join("count");
    let agent = CountingAgent(AtomicUsize::new(0));
    let script = format!(
        "n=$(test -f {0} && /bin/cat {0} || printf 0); n=$((n+1)); printf %s \"$n\" > {0}",
        counter.display()
    );
    let mut config = Config::default();
    config.review.enabled = true;
    config.review.verification = vec![
        verification("first", &script, BTreeMap::new(), 1_000),
        verification("second", &script, BTreeMap::new(), 1_000),
    ];

    let report = run(&agents(&agent), &config, repository.path());
    assert!(report.is_valid());
    assert_eq!(agent.0.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read_to_string(counter).unwrap(), "1");
    assert!(report
        .session_summary()
        .contains("deduplicated: reused session probe"));
}

#[test]
fn launch_denial_is_distinct_from_a_test_failure() {
    let repository = tempfile::tempdir().unwrap();
    let agent = CountingAgent(AtomicUsize::new(0));
    let mut check = verification("denied", "true", BTreeMap::new(), 1_000);
    check.argv = vec!["definitely-not-an-installed-preflight-tool".into()];
    let mut config = Config::default();
    config.review.verification = vec![check];

    let report = run(&agents(&agent), &config, repository.path());
    let denied = report
        .checks
        .iter()
        .find(|check| check.check_id == "verification.denied")
        .unwrap();
    assert_eq!(denied.status, PreflightStatus::EnvironmentDenied);
    assert!(denied.detail.contains("environment denied"));
}
