//! Regressions for PRD-068's attempt-local context failure blast radius.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use familiar_ai_agent::{
    AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult, IsolationCapability,
};
use familiar_ai_core::{AppPaths, Config};
use familiar_ai_daemon::drive::{drive, DriveTermination, DriveWarrant};
use familiar_ai_daemon::run::AgentSet;
use familiar_ai_storage::{Database, DriverRepository};

struct CountingAgent(AtomicUsize);

impl CodingAgent for CountingAgent {
    fn isolation_capability(&self) -> IsolationCapability {
        IsolationCapability::FreshProcessPerExecution
    }

    fn execute(
        &self,
        _request: ExecutionRequest<'_>,
        _output: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ExecutionResult {
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_tokens: Some(0),
            exit_code: Some(0),
            ..ExecutionResult::default()
        })
    }
}

fn git(repository: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repository)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .status()
        .unwrap();
    assert!(status.success());
}

fn legacy_prd(number: u32, body: &str) -> String {
    format!(
        "# PRD-{number:03}: Driver hygiene fixture\n\n\
         **Status:** Ready for implementation\n\n\
         ## Objective\n\n{body}\n\n\
         ## Acceptance Criteria\n\n1. Fixture criterion.\n\n\
         ## Expected Files\n\n- `src/file-{number}.rs`\n"
    )
}

#[test]
fn missing_wave_two_reference_retains_precisely_and_session_admits_the_next_prd() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir_all(repository.join("docs/prds")).unwrap();
    fs::create_dir_all(repository.join("docs/adr")).unwrap();
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(
        repository.join("docs/prds/PRD-049.md"),
        legacy_prd(49, "Requires docs/adr/missing-authoritative-input.md."),
    )
    .unwrap();
    fs::write(
        repository.join("docs/prds/PRD-050.md"),
        legacy_prd(50, "Contains no missing authoritative reference."),
    )
    .unwrap();
    git(&repository, &["init", "-q"]);
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "wave-two context fixture"]);

    let app = temp.path().join("app");
    let paths = AppPaths {
        config_dir: app.join("config"),
        data_dir: app.join("data"),
        state_dir: app.join("state"),
        runtime_dir: app.join("runtime"),
        log_dir: app.join("log"),
        socket_path: app.join("runtime/socket"),
        pid_path: app.join("state/pid"),
    };
    fs::create_dir_all(&paths.data_dir).unwrap();
    let mut config = Config::default();
    config.database.path = Some(paths.data_dir.join("familiar.db"));
    let agent = CountingAgent(AtomicUsize::new(0));
    let agents = AgentSet {
        implementation: &agent,
        reviewer: &agent,
        remediation: &agent,
    };

    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(&repository).unwrap();
    let summary = drive(
        &agents,
        &config,
        &paths,
        DriveWarrant {
            max_prds: 3,
            ..DriveWarrant::default()
        },
    )
    .unwrap();
    std::env::set_current_dir(previous).unwrap();

    assert_eq!(summary.attempted, 2);
    assert_eq!(summary.termination, DriveTermination::NothingEligible);
    assert_eq!(agent.0.load(Ordering::SeqCst), 1);

    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let records = DriverRepository::new(db.conn())
        .attempts(&summary.session_id)
        .unwrap();
    assert_eq!(records.len(), 2);
    let failed = records
        .iter()
        .find(|attempt| attempt.prd_path.ends_with("PRD-049.md"))
        .unwrap();
    assert_eq!(
        failed.retained_reason.as_deref(),
        Some("missing_authoritative_input_reference")
    );
    assert!(records
        .iter()
        .all(|attempt| attempt.retained_reason.as_deref() != Some("unclassified_result")));
}

#[test]
fn metadata_check_modes_name_their_exit_contract() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir_all(repository.join("docs/prds")).unwrap();
    fs::create_dir_all(repository.join("docs/prds/done")).unwrap();
    fs::write(
        repository.join("docs/prds/PRD-001.md"),
        legacy_prd(1, "Legacy migration debt."),
    )
    .unwrap();
    git(&repository, &["init", "-q"]);
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "metadata fixture"]);

    let run = |mode: &str| {
        Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
            .current_dir(&repository)
            .args(["backlog", "metadata-check", mode])
            .env("HOME", temp.path())
            .env("FAMILIAR_AI_DATABASE__PATH", temp.path().join("unused.db"))
            .env("XDG_RUNTIME_DIR", temp.path().join("xdg-runtime"))
            .output()
            .unwrap()
    };
    let advisory = run("--advisory");
    assert!(
        advisory.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&advisory.stdout),
        String::from_utf8_lossy(&advisory.stderr)
    );
    let advisory_stdout = String::from_utf8(advisory.stdout).unwrap();
    assert!(advisory_stdout.starts_with("metadata-check mode=advisory\n"));
    assert!(advisory_stdout.contains("mode=advisory structured_v1=0 legacy=1"));

    let strict = run("--strict");
    assert!(!strict.status.success());
    assert!(String::from_utf8(strict.stdout)
        .unwrap()
        .starts_with("metadata-check mode=strict\n"));
    assert!(String::from_utf8(strict.stderr)
        .unwrap()
        .contains("metadata-check mode=strict: 1 legacy PRD(s) require migration"));
}
