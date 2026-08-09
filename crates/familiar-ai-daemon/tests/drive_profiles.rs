//! PRD-027: a repository Familiar can read and verify must also be one it can
//! execute. These tests drive a temporary `numbered-slug` repository with a fake
//! agent, covering the three defects that made the first real spectra session
//! retain six PRDs in twenty-two milliseconds at zero cost.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use familiar_ai_agent::{
    AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult, IsolationCapability,
};
use familiar_ai_core::config::{ReviewAgentConfig, ReviewVerificationConfig};
use familiar_ai_core::{
    AppPaths, BacklogProfileName, Config, RepositoryEntryConfig, ReviewConfig,
    ScopeDeclarationModeConfig,
};
use familiar_ai_daemon::drive::{drive, DriveWarrant};
use familiar_ai_daemon::run::AgentSet;
use familiar_ai_storage::{Database, DriverRepository};

/// The driver resolves the backlog from the process working directory, so each
/// test owns it for the duration of its run.
static WORKING_DIRECTORY: Mutex<()> = Mutex::new(());

struct RecordingAgent {
    calls: Mutex<Vec<String>>,
}

impl CodingAgent for RecordingAgent {
    fn isolation_capability(&self) -> IsolationCapability {
        IsolationCapability::FreshProcessPerExecution
    }
    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        _output: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        let marker = request
            .prompt
            .lines()
            .find(|line| line.starts_with("## PRD: "))
            .unwrap_or("unknown")
            .to_owned();
        self.calls.lock().unwrap().push(marker);
        Ok(ExecutionResult {
            agent_version: Some("fake 1".into()),
            model: None,
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_tokens: Some(0),
            exit_code: Some(0),
            signal: None,
            session_id: None,
            reported_cost_microusd: None,
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
    assert!(status.success(), "git {args:?} failed");
}

/// A PRD in spectra's convention. Note there is no `## Expected Files` section
/// and there never can be: the grammar treats the body as opaque.
fn numbered_slug_prd(identity: &str, title: &str) -> String {
    format!(
        "# PRD {identity} — {title}\n\n\
         ## Objective\n\nFixture objective.\n\n\
         ## Acceptance Criteria\n\n1. Fixture criterion.\n"
    )
}

/// A temporary repository in the numbered-slug convention, described by the
/// operator's configuration exactly as a real one would be.
fn fixture(prds: &[(&str, &str)]) -> (tempfile::TempDir, AppPaths, Config) {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("docs/prd/todo")).unwrap();
    fs::create_dir_all(repository.join("docs/prd/done")).unwrap();
    fs::create_dir_all(repository.join("internal")).unwrap();
    fs::write(repository.join("internal/lib.go"), "package internal\n").unwrap();
    for (identity, slug) in prds {
        fs::write(
            repository.join(format!("docs/prd/todo/{identity}-{slug}.md")),
            numbered_slug_prd(identity, "Fixture"),
        )
        .unwrap();
    }
    git(&repository, &["init", "-q"]);
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "fixture"]);

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
    config.repositories.insert(
        repository.canonicalize().unwrap().to_string_lossy().into(),
        RepositoryEntryConfig {
            profile: BacklogProfileName::NumberedSlug,
            active_dir: Some("docs/prd/todo".into()),
            archived_dir: Some("docs/prd/done".into()),
            ..RepositoryEntryConfig::default()
        },
    );
    (temp, paths, config)
}

/// A review tree that authorizes by configured paths alone — the shape that
/// makes the Expected Files contract contribute no authority at all.
fn review_by_configured_paths() -> ReviewConfig {
    ReviewConfig {
        enabled: true,
        allowed_paths: vec!["internal/".into(), "docs/prd/".into()],
        max_total_cost_microusd: 5_000_000,
        // A trivially-passing check: this PRD is about admission, not about
        // what verification does once it is reached.
        verification: vec![ReviewVerificationConfig {
            check_id: "noop".into(),
            argv: vec!["/bin/true".into()],
            working_directory: ".".into(),
            timeout_ms: 60_000,
            required: true,
            path_prefixes: Vec::new(),
            environment: std::collections::BTreeMap::from([(
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin".to_string(),
            )]),
        }],
        implementation_agent: ReviewAgentConfig {
            adapter_id: "fake".into(),
            agent_id: "implementation".into(),
            provider: Some("fake".into()),
            model: None,
        },
        reviewer_agent: ReviewAgentConfig {
            adapter_id: "fake".into(),
            agent_id: "reviewer".into(),
            provider: Some("fake".into()),
            model: None,
        },
        ..ReviewConfig::default()
    }
}

fn agents(agent: &RecordingAgent) -> AgentSet<'_> {
    AgentSet {
        implementation: agent,
        reviewer: agent,
    }
}

fn with_working_directory<T>(repository: &Path, body: impl FnOnce() -> T) -> T {
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(repository).unwrap();
    let result = body();
    std::env::set_current_dir(previous).unwrap();
    result
}

fn warrant(max_prds: u64) -> DriveWarrant {
    DriveWarrant {
        max_prds,
        max_cost_microusd: 0,
        max_duration_ms: 0,
    }
}

/// Criterion 7: the path that produced six retentions now reaches the agent.
/// Criterion 1: a PRD with no Expected Files section is admitted when the
/// configuration makes the contract non-authoritative.
/// Criterion 5: identities are persisted and rendered in the repository's own
/// spelling.
#[test]
fn a_numbered_slug_drive_reaches_the_agent_and_records_its_own_spelling() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, mut config) = fixture(&[("0177a", "connector-adp"), ("0177", "umbrella")]);
    config.review = review_by_configured_paths();
    let agent = RecordingAgent {
        calls: Mutex::new(Vec::new()),
    };
    let summary = with_working_directory(&temp.path().join("repo"), || {
        drive(&agents(&agent), &config, &paths, warrant(1)).unwrap()
    });

    // The agent actually ran — this is the whole point. Before PRD-027 this
    // died at admission with zero invocations.
    assert_eq!(
        agent.calls.lock().unwrap().len(),
        1,
        "the agent was never invoked; admission still refuses the PRD"
    );
    assert_eq!(summary.attempted, 1);

    // The epic child sorts before its umbrella and is persisted in the
    // repository's own spelling, not the canonical one.
    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let session = DriverRepository::new(db.conn())
        .latest_session()
        .unwrap()
        .unwrap();
    let attempts = DriverRepository::new(db.conn())
        .attempts(&session.session_id)
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].prd_id, "PRD 0177a");
    assert_eq!(attempts[0].prd_path, "docs/prd/todo/0177a-connector-adp.md");
}

/// Criterion 2: the same PRD is refused when the operator made the contract an
/// authority source — and criterion 4: the refusal explains itself rather than
/// being recorded as `unrecorded`.
#[test]
fn a_required_contract_refuses_and_the_attempt_records_why() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, mut config) = fixture(&[("0177a", "connector-adp")]);
    let mut review = review_by_configured_paths();
    review.scope.declaration_mode = ScopeDeclarationModeConfig::ExpectedRequired;
    config.review = review;
    let agent = RecordingAgent {
        calls: Mutex::new(Vec::new()),
    };
    with_working_directory(&temp.path().join("repo"), || {
        drive(&agents(&agent), &config, &paths, warrant(1)).unwrap()
    });

    // Failed closed before the agent.
    assert!(agent.calls.lock().unwrap().is_empty());

    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let session = DriverRepository::new(db.conn())
        .latest_session()
        .unwrap()
        .unwrap();
    let attempts = DriverRepository::new(db.conn())
        .attempts(&session.session_id)
        .unwrap();
    let reason = attempts[0]
        .retained_reason
        .as_deref()
        .expect("an attempt that fails at admission must still say why");
    assert!(
        reason.contains("has no Expected Files contract") && reason.contains("expected_required"),
        "reason did not name the cause: {reason}"
    );
    // Bounded and single-line, so the morning report stays readable.
    assert!(!reason.contains('\n'));
}

/// Criterion 3: a section that is present but malformed stays fatal even when
/// the contract grants no authority — a contract offered is a contract honored.
#[test]
fn a_malformed_contract_is_fatal_even_when_it_grants_no_authority() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, mut config) = fixture(&[("0177a", "connector-adp")]);
    config.review = review_by_configured_paths();
    let repository = temp.path().join("repo");
    // A heading with a bullet carrying no inline-code path expression.
    fs::write(
        repository.join("docs/prd/todo/0177a-connector-adp.md"),
        "# PRD 0177a — Fixture\n\n## Expected Files\n\n- internal/\n",
    )
    .unwrap();
    let agent = RecordingAgent {
        calls: Mutex::new(Vec::new()),
    };
    with_working_directory(&repository, || {
        drive(&agents(&agent), &config, &paths, warrant(1)).unwrap()
    });

    assert!(agent.calls.lock().unwrap().is_empty());
    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let session = DriverRepository::new(db.conn())
        .latest_session()
        .unwrap()
        .unwrap();
    let attempts = DriverRepository::new(db.conn())
        .attempts(&session.session_id)
        .unwrap();
    let reason = attempts[0].retained_reason.as_deref().unwrap();
    assert!(
        reason.contains("Expected Files contract is invalid"),
        "reason did not name the malformed contract: {reason}"
    );
}
