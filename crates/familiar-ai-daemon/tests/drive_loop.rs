//! Unattended driver loop over a temporary backlog, using fake agents only.
//! Review stays disabled here: every attempt therefore retains its PRD exactly
//! as `run` does today, which is precisely what proves the loop keeps going,
//! records each attempt, and never re-selects an entry.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use familiar_ai_agent::{
    AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult, IsolationCapability,
};
use familiar_ai_core::{
    config::{
        RegistryWorkerConfig, WorkerCapabilityConfig, WorkerRegistryConfig, WorkerRouteRuleConfig,
    },
    AgentAdapterKind, AppPaths, Config,
};
use familiar_ai_daemon::drive::{drive, DriveTermination, DriveWarrant};
use familiar_ai_daemon::run::AgentSet;
use familiar_ai_storage::{Database, DriverRepository};

/// Serializes tests: the driver resolves the backlog from the process working
/// directory, so each test owns it for the duration of its run.
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
        // Record which PRD this invocation was compiled for.
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
            ..ExecutionResult::default()
        })
    }
}

struct ConcurrencyAgent {
    active: AtomicUsize,
    peak: AtomicUsize,
    preflights: AtomicUsize,
}

struct PanickingAgent;

impl CodingAgent for PanickingAgent {
    fn execute(
        &self,
        _request: ExecutionRequest<'_>,
        _output: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        panic!("simulated adapter panic")
    }
}

struct FailingPreflightAgent;

impl CodingAgent for FailingPreflightAgent {
    fn preflight(&self) -> Result<(), String> {
        Err("reviewer executable unavailable".into())
    }

    fn execute(
        &self,
        _request: ExecutionRequest<'_>,
        _output: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        panic!("preflight failure must prevent execution")
    }
}

impl CodingAgent for ConcurrencyAgent {
    fn preflight(&self) -> Result<(), String> {
        self.preflights.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn isolation_capability(&self) -> IsolationCapability {
        IsolationCapability::FreshProcessPerExecution
    }

    fn execute(
        &self,
        _request: ExecutionRequest<'_>,
        _output: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ExecutionResult {
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
    assert!(status.success(), "git {args:?} failed");
}

fn prd(number: u32, depends_on: Option<u32>) -> String {
    let dependency = depends_on
        .map(|n| format!("**Depends on:** PRD-{n:03}\n"))
        .unwrap_or_default();
    format!(
        "# PRD-{number:03}: Fixture {number}\n\n\
         **Status:** Ready for implementation\n{dependency}\n\
         ## Objective\n\nFixture objective.\n\n\
         ## Acceptance Criteria\n\n1. Fixture criterion.\n\n\
         ## Expected Files\n\n- `src/`\n"
    )
}

/// A temp git repository with `count` PRDs plus app paths and a config.
fn fixture(count: u32, chained: bool) -> (tempfile::TempDir, AppPaths, Config) {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("docs/prds")).unwrap();
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(repository.join("src/lib.rs"), "// fixture\n").unwrap();
    for number in 1..=count {
        let depends_on = if chained && number > 1 {
            Some(number - 1)
        } else {
            None
        };
        fs::write(
            repository.join(format!("docs/prds/PRD-{number:03}.md")),
            prd(number, depends_on),
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
    (temp, paths, config)
}

fn agents(agent: &RecordingAgent) -> AgentSet<'_> {
    AgentSet {
        implementation: agent,
        reviewer: agent,
        remediation: agent,
    }
}

fn with_working_directory<T>(repository: &Path, body: impl FnOnce() -> T) -> T {
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(repository).unwrap();
    let result = body();
    std::env::set_current_dir(previous).unwrap();
    result
}

fn permissions_fixup(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn write_executable(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    permissions_fixup(path);
}

#[test]
fn warrant_without_a_finite_ceiling_is_refused_before_any_session_opens() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, config) = fixture(1, false);
    let agent = RecordingAgent {
        calls: Mutex::new(Vec::new()),
    };
    let error = with_working_directory(&temp.path().join("repo"), || {
        drive(&agents(&agent), &config, &paths, DriveWarrant::default()).unwrap_err()
    });
    assert!(error.to_string().contains("finite ceiling"));
    // No session row exists: the refusal happened before storage was touched.
    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    db.run_migrations().unwrap();
    assert!(DriverRepository::new(db.conn())
        .latest_session()
        .unwrap()
        .is_none());
    assert!(agent.calls.lock().unwrap().is_empty());
}

#[test]
fn loop_attempts_every_independent_prd_once_and_records_each() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, config) = fixture(3, false);
    let agent = RecordingAgent {
        calls: Mutex::new(Vec::new()),
    };
    let summary = with_working_directory(&temp.path().join("repo"), || {
        drive(
            &agents(&agent),
            &config,
            &paths,
            DriveWarrant {
                max_prds: 10,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });

    // Review is disabled, so each attempt retains its PRD; the loop continues
    // regardless and stops only when nothing is left to select.
    assert_eq!(summary.attempted, 3);
    assert_eq!(summary.completed, 0);
    assert_eq!(summary.termination, DriveTermination::NothingEligible);
    assert_eq!(agent.calls.lock().unwrap().len(), 3);

    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let repository = DriverRepository::new(db.conn());
    let session = repository.latest_session().unwrap().unwrap();
    assert_eq!(session.session_id, summary.session_id);
    assert_eq!(
        session.termination_reason.as_deref(),
        Some("nothing_eligible")
    );
    assert!(session.ended_at.is_some());

    let attempts = repository.attempts(&summary.session_id).unwrap();
    assert_eq!(attempts.len(), 3);
    assert_eq!(
        attempts.iter().map(|a| a.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    // Selection follows PRD number order.
    assert_eq!(attempts[0].prd_path, "docs/prds/PRD-001.md");
    assert_eq!(attempts[2].prd_path, "docs/prds/PRD-003.md");
    for attempt in &attempts {
        assert_eq!(attempt.outcome.as_deref(), Some("retained"));
        assert_eq!(attempt.retained_reason.as_deref(), Some("review_disabled"));
        assert!(attempt.ended_at.is_some());
        assert!(attempt.duration_ms.is_some());
        // No pricing configured: cost stays unknown, never zero.
        assert_eq!(attempt.known_cost_microusd, None);
    }
}

#[test]
fn legacy_prd_document_scope_routes_and_persists_the_derived_file_count() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, mut config) = fixture(1, false);
    let repository = temp.path().join("repo");
    let prd_path = repository.join("docs/prds/PRD-001.md");
    let content = fs::read_to_string(&prd_path).unwrap().replace(
        "- `src/`",
        "- `src/one.rs`\n- `src/two.rs`\n- `src/three.rs`",
    );
    fs::write(&prd_path, content).unwrap();
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "three-file legacy scope"]);

    let worker = temp.path().join("worker");
    write_executable(
        &worker,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo codex-test; exit 0; fi\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"turn.completed\"}'\n",
    );
    let registry_worker = |model: &str| RegistryWorkerConfig {
        adapter: Some(AgentAdapterKind::Codex),
        provider: "test".into(),
        model: model.into(),
        runtime: None,
        model_artifact: None,
        auth_profile: None,
        capability_profile: None,
        runtime_config: None,
        executable: Some(worker.to_string_lossy().into_owned()),
        capabilities: vec![
            WorkerCapabilityConfig::Implementation,
            WorkerCapabilityConfig::Remediation,
        ],
        fresh_process_isolation: true,
        context_tokens: 100_000,
        estimated_cost_microusd: 1,
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: Vec::new(),
    };
    let mut registry = WorkerRegistryConfig::default();
    registry
        .workers
        .insert("small".into(), registry_worker("small"));
    registry
        .workers
        .insert("large".into(), registry_worker("large"));
    registry.routing.rules = vec![
        WorkerRouteRuleConfig {
            id: "one-file".into(),
            worker: "small".into(),
            risk_classes: Vec::new(),
            max_expected_files: Some(1),
        },
        WorkerRouteRuleConfig {
            id: "three-files".into(),
            worker: "large".into(),
            risk_classes: Vec::new(),
            max_expected_files: Some(3),
        },
    ];
    config.worker_registry = Some(registry);

    let fallback = RecordingAgent {
        calls: Mutex::new(Vec::new()),
    };
    let summary = with_working_directory(&repository, || {
        drive(
            &agents(&fallback),
            &config,
            &paths,
            DriveWarrant {
                max_prds: 1,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });
    assert_eq!(summary.attempted, 1);

    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let stored: (String, String, u64) = db
        .conn()
        .query_row(
            "SELECT rule, selected_identity, expected_file_count FROM worker_selections WHERE stage = 'implementation'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored.0, "user-pin");
    assert!(stored.1.starts_with("wspec-sha256:"));
    assert_eq!(stored.2, 3);
}

#[test]
fn prd_count_ceiling_stops_the_session_before_the_next_attempt() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, config) = fixture(3, false);
    let agent = RecordingAgent {
        calls: Mutex::new(Vec::new()),
    };
    let summary = with_working_directory(&temp.path().join("repo"), || {
        drive(
            &agents(&agent),
            &config,
            &paths,
            DriveWarrant {
                max_prds: 2,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });
    assert_eq!(summary.attempted, 2);
    assert_eq!(summary.termination, DriveTermination::BudgetPrdsExhausted);
    assert_eq!(agent.calls.lock().unwrap().len(), 2);
}

#[test]
fn a_retained_prd_makes_its_dependents_ineligible_without_stopping_the_session() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    // PRD-002 depends on PRD-001, which will be retained (review disabled).
    let (temp, paths, config) = fixture(2, true);
    let agent = RecordingAgent {
        calls: Mutex::new(Vec::new()),
    };
    let summary = with_working_directory(&temp.path().join("repo"), || {
        drive(
            &agents(&agent),
            &config,
            &paths,
            DriveWarrant {
                max_prds: 10,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });
    // Only the independent PRD is attempted; its dependent is skipped because
    // the dependency never reached `completed`.
    assert_eq!(summary.attempted, 1);
    assert_eq!(summary.termination, DriveTermination::NothingEligible);
    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let attempts = DriverRepository::new(db.conn())
        .attempts(&summary.session_id)
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].prd_path, "docs/prds/PRD-001.md");
}

#[test]
fn an_empty_backlog_terminates_immediately_without_attempts() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("docs/prds")).unwrap();
    fs::write(repository.join("README.md"), "no prds\n").unwrap();
    git(&repository, &["init", "-q"]);
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "empty"]);
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
    let agent = RecordingAgent {
        calls: Mutex::new(Vec::new()),
    };
    let summary = with_working_directory(&repository, || {
        drive(
            &agents(&agent),
            &config,
            &paths,
            DriveWarrant {
                max_prds: 5,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });
    assert_eq!(summary.attempted, 0);
    assert_eq!(summary.termination, DriveTermination::BacklogEmpty);
    assert!(agent.calls.lock().unwrap().is_empty());
    let _ = permissions_fixup;
}

#[test]
fn a_cost_ceiling_with_unknown_cost_ends_the_session_after_one_attempt() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    // No [execution_history.pricing] is configured, so every attempt's cost is
    // unknown — the honest, fail-closed outcome is to stop rather than treat
    // an unmeasurable attempt as free.
    let (temp, paths, config) = fixture(3, false);
    let agent = RecordingAgent {
        calls: Mutex::new(Vec::new()),
    };
    let summary = with_working_directory(&temp.path().join("repo"), || {
        drive(
            &agents(&agent),
            &config,
            &paths,
            DriveWarrant {
                max_cost_microusd: 1_000_000,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });
    assert_eq!(summary.attempted, 1);
    assert_eq!(summary.termination, DriveTermination::CostUnknown);
    assert_eq!(summary.known_cost_microusd, 0);
}

#[test]
fn independent_scopes_execute_with_bounded_parallelism() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, mut config) = fixture(2, false);
    let repository = temp.path().join("repo");
    for number in 1..=2 {
        fs::create_dir_all(repository.join(format!("component-{number}"))).unwrap();
        fs::write(
            repository.join(format!("component-{number}/fixture.txt")),
            "fixture\n",
        )
        .unwrap();
        let path = repository.join(format!("docs/prds/PRD-{number:03}.md"));
        let content = fs::read_to_string(&path)
            .unwrap()
            .replace("- `src/`", &format!("- `component-{number}/`"));
        fs::write(path, content).unwrap();
    }
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "independent scopes"]);
    config.driver.max_concurrency = 2;
    config.driver.max_parallel_components = 2;
    config.driver.isolated_worktrees = true;
    config.agents = Some(familiar_ai_core::config::AgentsConfig::default());
    let agent = ConcurrencyAgent {
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        preflights: AtomicUsize::new(0),
    };
    let agents = AgentSet {
        implementation: &agent,
        reviewer: &agent,
        remediation: &agent,
    };
    let summary = with_working_directory(&repository, || {
        drive(
            &agents,
            &config,
            &paths,
            DriveWarrant {
                max_prds: 2,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });
    assert_eq!(summary.attempted, 2);
    assert_eq!(agent.peak.load(Ordering::SeqCst), 2);
    assert_eq!(agent.preflights.load(Ordering::SeqCst), 1);
    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let attempts = DriverRepository::new(db.conn())
        .attempts(&summary.session_id)
        .unwrap();
    assert!(attempts.iter().all(|attempt| attempt.model.is_none()));
}

#[test]
fn panicked_worker_is_terminalized_before_the_session_stops() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, mut config) = fixture(1, false);
    config.driver.isolated_worktrees = true;
    let agent = PanickingAgent;
    let agents = AgentSet {
        implementation: &agent,
        reviewer: &agent,
        remediation: &agent,
    };
    let summary = with_working_directory(&temp.path().join("repo"), || {
        drive(
            &agents,
            &config,
            &paths,
            DriveWarrant {
                max_prds: 1,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });
    assert_eq!(summary.termination, DriveTermination::UnclassifiedResult);
    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let attempts = DriverRepository::new(db.conn())
        .attempts(&summary.session_id)
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert!(attempts[0].ended_at.is_some());
    assert_eq!(attempts[0].outcome.as_deref(), Some("retained"));
    assert_eq!(
        attempts[0].retained_reason.as_deref(),
        Some("unclassified_result")
    );
}

#[test]
fn preflight_failure_detail_is_durable_without_claiming() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, config) = fixture(1, false);
    let agent = FailingPreflightAgent;
    let agents = AgentSet {
        implementation: &agent,
        reviewer: &agent,
        remediation: &agent,
    };
    let summary = with_working_directory(&temp.path().join("repo"), || {
        drive(
            &agents,
            &config,
            &paths,
            DriveWarrant {
                max_prds: 1,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });
    assert_eq!(summary.termination, DriveTermination::PreflightFailed);
    assert_eq!(summary.attempted, 0);
    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let session = DriverRepository::new(db.conn())
        .get_session(&summary.session_id)
        .unwrap()
        .unwrap();
    assert!(session
        .termination_detail
        .as_deref()
        .is_some_and(|detail| detail.contains("reviewer executable unavailable")));
}
