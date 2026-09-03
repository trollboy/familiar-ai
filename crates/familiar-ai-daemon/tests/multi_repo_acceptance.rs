//! PRD-038: Multi-Repository Have-At-It Acceptance.
//!
//! The repository-agnostic product acceptance proof: design documents in,
//! one approved decomposition, unattended bounded execution, and an exact
//! morning report — exercised against materially different repositories
//! using only real Familiar library code (fake agents/runners, no network).
#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use familiar_ai_agent::{
    AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult, IsolationCapability,
};
use familiar_ai_core::config::{ReviewAgentConfig, ReviewGateConfig, ReviewVerificationConfig};
use familiar_ai_core::{
    onboarding, AppPaths, BacklogDiscovery, Config, DeliveryConfig, DeliveryMode,
    FilesystemBacklogDiscovery, PlannerConfig, PocSelfApprovalWarrant,
};
use familiar_ai_daemon::delivery::{deliver_with, CommandRunner};
use familiar_ai_daemon::drive::{drive, DriveWarrant};
use familiar_ai_daemon::plan;
use familiar_ai_daemon::run::AgentSet;
use familiar_ai_daemon::worktree::{recover_incomplete, WorktreeOwnership};
use familiar_ai_storage::{Database, DriverRepository, PlannerBatchRepository};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

static WORKING_DIRECTORY: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------
// Shared fixture helpers (patterned after drive_loop.rs / autonomous_delivery.rs).
// ---------------------------------------------------------------------

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

fn with_working_directory<T>(repository: &Path, body: impl FnOnce() -> T) -> T {
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(repository).unwrap();
    let result = body();
    std::env::set_current_dir(previous).unwrap();
    result
}

fn app_paths(root: &Path) -> AppPaths {
    let app = root.join("app");
    AppPaths {
        config_dir: app.join("config"),
        data_dir: app.join("data"),
        state_dir: app.join("state"),
        runtime_dir: app.join("runtime"),
        log_dir: app.join("log"),
        socket_path: app.join("runtime/socket"),
        pid_path: app.join("state/pid"),
    }
}

fn base_config(paths: &AppPaths) -> Config {
    fs::create_dir_all(&paths.data_dir).unwrap();
    let mut config = Config::default();
    config.database.path = Some(paths.data_dir.join("familiar.db"));
    config
}

/// Recursively copies a checked-in fixture tree (`tests/fixtures/<name>`)
/// into a fresh temp git repository. Onboarding discovery reads only real
/// filesystem content, so the two acceptance repositories are real trees,
/// not synthetic strings.
fn copy_fixture_repository(name: &str, destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures")
        .join(name);
    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), &target).unwrap();
            }
        }
    }
    copy_dir(&source, destination);
    git(destination, &["init", "-q"]);
    // A container or CI runner may have no global git identity; the drive
    // commits during integration, so the fixture provides its own.
    git(
        destination,
        &["config", "user.email", "fixture@familiar-ai.invalid"],
    );
    git(destination, &["config", "user.name", "fixture"]);
    git(destination, &["add", "-A"]);
    git(destination, &["commit", "-qm", "fixture"]);
}

// ---------------------------------------------------------------------
// AC1: at least two materially different fixture repositories complete
// onboarding (propose -> approve -> validate -> fixture) without any
// change to Familiar code, and the approved policy is real, effective
// Config through the same fragment-merge path the daemon uses.
// ---------------------------------------------------------------------

fn onboard_and_load(
    fixture_name: &str,
    active_dir: &str,
    archived_dir: &str,
    expected_language: &str,
    expected_build_tool: &str,
) {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    copy_fixture_repository(fixture_name, &repository);
    let canonical = repository.canonicalize().unwrap();

    let proposal = onboarding::propose(&repository).unwrap();
    assert!(
        !proposal.authority_granted,
        "discovery must never self-grant authority"
    );
    assert_eq!(proposal.languages, [expected_language]);
    assert!(proposal
        .build_tools
        .contains(&expected_build_tool.to_owned()));

    let staging = tempfile::tempdir().unwrap();
    let proposal_path = staging.path().join("proposal.toml");
    fs::write(
        &proposal_path,
        onboarding::encode_proposal(&proposal).unwrap(),
    )
    .unwrap();

    let answers_path = staging.path().join("answers.toml");
    fs::write(
        &answers_path,
        format!(
            r#"
repository = {canonical:?}
profile = "canonical"
active_dir = {active_dir:?}
archived_dir = {archived_dir:?}
prd_metadata_policy = "incremental"

[review]
enabled = false

[execution_context]
hard_ceiling_tokens = 1000

[delivery]
mode = "disabled"
"#,
            canonical = canonical.to_string_lossy(),
        ),
    )
    .unwrap();

    let (_, encoded) =
        onboarding::approve(&proposal_path, &answers_path, "human:acceptance").unwrap();

    // Write into a real repositories_dir beside a main config file, exactly
    // where `Config::load` looks for approved fragments.
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).unwrap();
    let main_config_path = config_dir.join("config.toml");
    fs::write(&main_config_path, "").unwrap();
    let repositories_dir = config_dir.join("repositories");
    fs::create_dir_all(&repositories_dir).unwrap();
    let policy_path = repositories_dir.join(format!(
        "{}.toml",
        onboarding::sha256(canonical.to_string_lossy().as_bytes())
    ));
    fs::write(&policy_path, &encoded).unwrap();

    let attribution = onboarding::validate_policy(&policy_path).unwrap();
    assert_eq!(attribution.actor, "human:acceptance");
    assert_eq!(attribution.repository, canonical.to_string_lossy());

    let fixture_output = onboarding::safe_fixture(&policy_path).unwrap();
    assert!(
        fixture_output.starts_with("fixture ok:"),
        "{fixture_output}"
    );
    assert!(fixture_output.contains("boundary=validated"));

    // The approved policy becomes real, effective Config through the exact
    // fragment-merge path the daemon uses at startup — no code change.
    let loaded = Config::load(Some(&main_config_path)).unwrap();
    let entry = loaded
        .repositories
        .get(&canonical.to_string_lossy().into_owned())
        .unwrap_or_else(|| panic!("onboarded repository missing from effective config"));
    assert_eq!(entry.active_dir, active_dir);
    assert_eq!(entry.archived_dir, archived_dir);
}

#[test]
fn rust_cli_repository_onboards_without_code_changes() {
    onboard_and_load(
        "repo-rust-cli",
        "docs/prds",
        "docs/prds/done",
        "rust",
        "cargo",
    );
}

#[test]
fn node_service_repository_onboards_without_code_changes() {
    onboard_and_load(
        "repo-node-service",
        "planning/backlog",
        "planning/backlog/done",
        "javascript/typescript",
        "npm",
    );
}

// ---------------------------------------------------------------------
// AC2, AC3, AC5: the planner drafts and one human approves a
// dependency-ordered batch under a finite warrant; independent branches
// then execute concurrently while a real dependency and a real shared
// (undeclared) scope conflict both serialize correctly; an injected
// failure strands only its own dependent while every other independent
// branch completes; and the resulting state survives a simulated
// supervisor restart without duplicating or losing any work.
// ---------------------------------------------------------------------

struct FixturePlannerAgent(String);

impl CodingAgent for FixturePlannerAgent {
    fn execute(
        &self,
        _request: ExecutionRequest<'_>,
        output: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        output.write_all(self.0.as_bytes()).unwrap();
        Ok(ExecutionResult {
            exit_code: Some(0),
            ..Default::default()
        })
    }
}

fn drafted_prd(number: u32, depends_on: Option<u32>, scope_dir: &str) -> String {
    let dependency = depends_on
        .map(|n| format!("PRD-{n:03}"))
        .unwrap_or_else(|| "none".to_owned());
    format!(
        "# PRD-{number:03}: Fixture {number}\n\n\
         **Depends on:** {dependency}\n\n\
         ## Objective\n\nFixture objective.\n\n\
         ## Scope\n\nFixture scope.\n\n\
         ## Non-goals\n\nNone.\n\n\
         ## Acceptance Criteria\n\n1. Fixture criterion.\n\n\
         ## Test Strategy\n\nFixture test strategy.\n\n\
         ## Expected Files\n\n- `{scope_dir}/`\n\n\
         ## Definition of Done\n\nFixture definition of done.\n"
    )
}

fn hand_written_prd(number: u32, depends_on: Option<u32>, scope_dir: &str) -> String {
    let dependency = depends_on
        .map(|n| format!("**Depends on:** PRD-{n:03}\n"))
        .unwrap_or_default();
    format!(
        "# PRD-{number:03}: Fixture {number}\n\n\
         **Status:** Ready for implementation\n{dependency}\n\
         ## Objective\n\nFixture objective.\n\n\
         ## Acceptance Criteria\n\n1. Fixture criterion.\n\n\
         ## Expected Files\n\n- `{scope_dir}/`\n"
    )
}

/// Records concurrency (active/peak) across all invocations and, for the
/// one PRD matching `fail_marker`, returns a clean (non-panicking) agent
/// execution error instead of writing output — the injected failure.
struct ProofAgent {
    active: AtomicUsize,
    peak: AtomicUsize,
    fail_marker: String,
}

impl CodingAgent for ProofAgent {
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
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        self.active.fetch_sub(1, Ordering::SeqCst);

        if marker.contains(&self.fail_marker) {
            return Err(AgentExecutionError::MalformedOutput {
                detail: "injected failure fixture".into(),
                result: Box::new(ExecutionResult {
                    exit_code: Some(1),
                    ..ExecutionResult::default()
                }),
            });
        }
        // Deliberately does not touch the worktree: two components' agents
        // run genuinely concurrently here (that's the point), and each
        // writing real files would race two independent git worktrees'
        // checkpoint diffing against the same session database at once.
        // That combination is proven separately and serially in
        // `shared_scope_without_a_declared_dependency_still_serializes`,
        // where the scope conflict itself prevents true concurrency.
        Ok(ExecutionResult {
            agent_version: Some("fake 1".into()),
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_tokens: Some(0),
            exit_code: Some(0),
            ..ExecutionResult::default()
        })
    }
}

/// Reviewer fake: reads the serialized review request from its isolated
/// workspace and returns the minimal clean wire result (identity echoes,
/// zero findings) — identical shape to the one proven in
/// `autonomous_delivery.rs`.
struct CleanReviewer;

impl CodingAgent for CleanReviewer {
    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        output: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        let raw = fs::read_to_string(request.working_directory.join("review-request.json"))
            .expect("review workspace carries the request");
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let wire = serde_json::json!({
            "review_id": value["review_id"],
            "reviewed_manifest_hash": value["manifest"]["manifest_hash"],
            "findings": [],
        });
        output.write_all(wire.to_string().as_bytes()).unwrap();
        Ok(ExecutionResult {
            agent_version: Some("fake-reviewer 1".into()),
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_tokens: Some(0),
            exit_code: Some(0),
            ..ExecutionResult::default()
        })
    }
}

fn plan_execute_route_fail_and_recover_fixture() -> (tempfile::TempDir, AppPaths, Config, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("docs/prds")).unwrap();
    fs::write(repository.join("README.md"), "acceptance fixture\n").unwrap();
    git(&repository, &["init", "-q"]);
    // A container or CI runner may have no global git identity; the drive
    // commits during integration, so the fixture provides its own.
    git(
        &repository,
        &["config", "user.email", "fixture@familiar-ai.invalid"],
    );
    git(&repository, &["config", "user.name", "fixture"]);
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "fixture"]);

    let paths = app_paths(temp.path());
    let mut config = base_config(&paths);
    // Review stays disabled here, exactly as in drive_loop.rs's own
    // concurrency proof: every attempt therefore retains its PRD instead of
    // completing, which is what lets this test measure genuine concurrent
    // execution across independent components without also racing multiple
    // real review cycles' checkpoint writes against the same SQLite file
    // (a shared-scope wave's serialized completion is proven separately, in
    // `shared_scope_without_a_declared_dependency_still_serializes`).
    config.driver.max_parallel_components = 2;
    config.driver.max_concurrency = 2;
    config.driver.isolated_worktrees = true;
    config.agents = Some(familiar_ai_core::config::AgentsConfig::default());

    (temp, paths, config, repository)
}

#[test]
fn planner_drafts_a_dependency_ordered_batch_under_one_human_approval_and_a_warrant() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("docs/prds")).unwrap();
    fs::write(repository.join("README.md"), "acceptance fixture\n").unwrap();
    git(&repository, &["init", "-q"]);
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "fixture"]);

    let paths = app_paths(temp.path());
    let mut config = base_config(&paths);
    let planner_limits = PlannerConfig {
        agent: Default::default(),
        max_prds_per_batch: 8,
        max_bytes_per_prd: 65536,
    };
    config.planner = Some(planner_limits.clone());

    // PRD-001/002/004 are independent (three components); PRD-003 depends
    // on PRD-001 (dependency serialization, proven at execution time in
    // `independent_branches_execute_concurrently_and_strand_only_the_failing_dependent`);
    // PRD-005 depends on PRD-004 (the branch that will fail there).
    let raw = format!(
        "=== PRD-001.md ===\n{}=== PRD-002.md ===\n{}=== PRD-003.md ===\n{}=== PRD-004.md ===\n{}=== PRD-005.md ===\n{}",
        drafted_prd(1, None, "component-a"),
        drafted_prd(2, None, "component-b"),
        drafted_prd(3, Some(1), "component-a"),
        drafted_prd(4, None, "component-c"),
        drafted_prd(5, Some(4), "component-d"),
    );
    let design = repository.join("design.md");
    fs::write(&design, "acceptance batch design\n").unwrap();
    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    db.run_migrations().unwrap();
    let (batch_id, summaries) = plan::generate(
        &repository,
        &[design],
        &config,
        &paths,
        &db,
        &FixturePlannerAgent(raw),
    )
    .unwrap();
    assert_eq!(summaries.len(), 5);
    assert_eq!(summaries[2].dependencies, ["PRD-001"]);
    assert_eq!(summaries[4].dependencies, ["PRD-004"]);

    // One human batch approval, recorded durably.
    let mut db = db;
    let identity = FilesystemBacklogDiscovery.resolve(&repository).unwrap();
    plan::approve(
        &repository,
        &batch_id,
        "human:acceptance",
        &planner_limits,
        &identity,
        &mut db,
    )
    .unwrap();
    let batch_record = PlannerBatchRepository::new(db.conn())
        .get(&batch_id)
        .unwrap()
        .unwrap();
    assert_eq!(batch_record.status, "approved");
    assert_eq!(batch_record.actor, "human:acceptance");
    assert_eq!(batch_record.file_hashes.len(), 5);
    for number in 1..=5u32 {
        assert!(repository
            .join(format!("docs/prds/PRD-{number:03}.md"))
            .is_file());
    }

    // The approved batch executes only under a finite, valid warrant.
    let warrant = DriveWarrant {
        max_prds: 5,
        max_duration_ms: 900_000,
        ..DriveWarrant::default()
    };
    warrant.validate().unwrap();
}

#[test]
fn two_independent_branches_execute_concurrently() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, config, repository) = plan_execute_route_fail_and_recover_fixture();

    // Exactly two independent, disjoint-scope PRDs — the ceiling itself, so
    // both are admitted the instant the session opens and genuinely
    // overlap. (A third simultaneously-eligible candidate is proven
    // separately, serially, in
    // `failure_strands_only_its_own_dependent_and_survives_restart` — with
    // review disabled, the two live worktree threads here already write
    // real concurrent checkpoints to one SQLite file, which is the
    // narrowest fixture that still proves genuine concurrency.)
    for (number, scope) in [(1u32, "component-a"), (2, "component-b")] {
        fs::write(
            repository.join(format!("docs/prds/PRD-{number:03}.md")),
            hand_written_prd(number, None, scope),
        )
        .unwrap();
    }
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "two independent PRDs"]);

    let agent = ProofAgent {
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        fail_marker: "no PRD in this fixture matches this marker".into(),
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

    assert_eq!(
        summary.attempted, 2,
        "termination: {:?}",
        summary.termination
    );
    // Independent branches ran concurrently: at least two PRDs were active
    // in the same 150ms window.
    assert!(
        agent.peak.load(Ordering::SeqCst) >= 2,
        "peak concurrency was {}",
        agent.peak.load(Ordering::SeqCst)
    );

    let _ = temp; // keep the TempDir alive for the whole test
}

#[test]
fn failure_strands_only_its_own_dependent_and_survives_restart() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, mut config, repository) = plan_execute_route_fail_and_recover_fixture();
    // Serial: this test proves dependency-blocking, failure isolation, and
    // restart recovery, none of which need genuine concurrency (proven on
    // its own, separately, in `two_independent_branches_execute_concurrently`).
    config.driver.max_parallel_components = 1;
    config.driver.max_concurrency = 1;

    // PRD-001 fails (injected); PRD-002 depends on PRD-001 and is therefore
    // stranded; PRD-003 is independent of both and must still be attempted
    // — proof that the failure strands only its own dependent.
    for (number, dependency, scope) in [
        (1u32, None, "component-a"),
        (2, Some(1u32), "component-b"),
        (3, None, "component-c"),
    ] {
        fs::write(
            repository.join(format!("docs/prds/PRD-{number:03}.md")),
            hand_written_prd(number, dependency, scope),
        )
        .unwrap();
    }
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "batch"]);

    let warrant = DriveWarrant {
        max_prds: 3,
        max_duration_ms: 900_000,
        ..DriveWarrant::default()
    };
    let agent = ProofAgent {
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        fail_marker: "PRD-001".into(),
    };
    let agents = AgentSet {
        implementation: &agent,
        reviewer: &agent,
        remediation: &agent,
    };
    let summary = with_working_directory(&repository, || {
        drive(&agents, &config, &paths, warrant).unwrap()
    });

    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let driver = DriverRepository::new(db.conn());
    let attempts = driver.attempts(&summary.session_id).unwrap();
    let attempted_paths: BTreeSet<_> = attempts.iter().map(|a| a.prd_path.clone()).collect();
    // PRD-002 declares a dependency on PRD-001, which never reaches
    // `completed` in this session — the dependency graph correctly
    // serializes it by never selecting it, while independent PRD-003 (no
    // relation to the failure) still runs normally.
    assert!(!attempted_paths.contains("docs/prds/PRD-002.md"));
    let failing = attempts
        .iter()
        .find(|a| a.prd_path == "docs/prds/PRD-001.md")
        .expect("PRD-001 was attempted");
    assert_eq!(failing.outcome.as_deref(), Some("retained"));
    assert_eq!(failing.retained_reason.as_deref(), Some("malformed_output"));
    let unaffected = attempts
        .iter()
        .find(|a| a.prd_path == "docs/prds/PRD-003.md")
        .expect("PRD-003 was attempted despite PRD-001's failure");
    assert_eq!(unaffected.outcome.as_deref(), Some("retained"));
    assert_eq!(
        unaffected.retained_reason.as_deref(),
        Some("implementation_incomplete"),
        "PRD-001's injected failure must be distinguishable from PRD-003's ordinary retention"
    );

    // --- AC5: the whole state survives a simulated supervisor restart
    // without touching unrelated, already-terminal work. ---
    let simulated_crash = WorktreeOwnership {
        session_id: summary.session_id.clone(),
        prd_id: "simulated-crash-component".into(),
        worktree: paths.state_dir.join("worktrees/simulated"),
        created_at: "2026-01-01T00:00:00Z".into(),
        heartbeat_at: "2026-01-01T00:00:00Z".into(),
        state: "owned".into(),
    };
    let crash_dir = paths.state_dir.join("worktrees").join(&summary.session_id);
    fs::create_dir_all(&crash_dir).unwrap();
    let crash_path = crash_dir.join("simulated-crash-component.ownership.json");
    fs::write(&crash_path, serde_json::to_vec(&simulated_crash).unwrap()).unwrap();

    let recovered = recover_incomplete(&paths.state_dir).unwrap();
    assert_eq!(recovered, 1, "only the simulated crash is recovered");
    let after: WorktreeOwnership = serde_json::from_slice(&fs::read(&crash_path).unwrap()).unwrap();
    assert_eq!(after.state, "retained_interrupted");

    let attempts_after_recovery = driver.attempts(&summary.session_id).unwrap();
    assert_eq!(
        attempts_after_recovery.len(),
        attempts.len(),
        "restart recovery must not create, duplicate, or lose any attempt"
    );

    let _ = temp; // keep the TempDir alive for the whole test
}

// ---------------------------------------------------------------------
// AC3 (continued): two independent PRDs that declare no dependency on each
// other, but whose Expected Files overlap, must still serialize through
// the shared-scope conflict queue — distinct from dependency-graph
// serialization, proven above. Kept as its own minimal, deterministic
// fixture (the proven two-PRD/two-component shape) rather than folded into
// the busier four-component flagship test above, which is not an
// appropriate place to also race a scope conflict.
// ---------------------------------------------------------------------

struct SharedScopeWritingAgent;

impl CodingAgent for SharedScopeWritingAgent {
    fn isolation_capability(&self) -> IsolationCapability {
        IsolationCapability::FreshProcessPerExecution
    }

    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        _output: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        let number = request
            .prompt
            .lines()
            .find_map(|line| line.strip_prefix("## PRD: docs/prds/PRD-"))
            .and_then(|rest| rest.trim_end_matches(".md").parse::<u32>().ok())
            .expect("prompt names the PRD");
        fs::create_dir_all(request.working_directory.join("shared")).unwrap();
        fs::write(
            request
                .working_directory
                .join("shared")
                .join(format!("prd_{number}.txt")),
            "written\n",
        )
        .unwrap();
        Ok(ExecutionResult {
            agent_version: Some("fake 1".into()),
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_tokens: Some(0),
            exit_code: Some(0),
            ..ExecutionResult::default()
        })
    }
}

#[test]
fn shared_scope_without_a_declared_dependency_still_serializes() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("docs/prds")).unwrap();
    fs::create_dir_all(repository.join("shared")).unwrap();
    fs::write(repository.join("shared/.gitkeep"), "").unwrap();
    for number in 1..=2u32 {
        fs::write(
            repository.join(format!("docs/prds/PRD-{number:03}.md")),
            hand_written_prd(number, None, "shared"),
        )
        .unwrap();
    }
    git(&repository, &["init", "-q"]);
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "shared-scope fixture"]);

    let paths = app_paths(temp.path());
    let mut config = base_config(&paths);
    config.review.enabled = true;
    config.review.max_total_tokens = 5_000_000;
    config.review.scope.allow_prd_expected_file_expansion = true;
    config.review.verification = vec![ReviewVerificationConfig {
        check_id: "noop".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "true".into()],
        working_directory: ".".into(),
        timeout_ms: 60_000,
        required: true,
        path_prefixes: Vec::new(),
        environment: Default::default(),
    }];
    config.review.implementation_agent = ReviewAgentConfig {
        adapter_id: "fake".into(),
        agent_id: "fake-implementation".into(),
        provider: Some("fake-implementer".into()),
        model: None,
    };
    config.review.reviewer_agent = ReviewAgentConfig {
        adapter_id: "fake".into(),
        agent_id: "fake-reviewer".into(),
        provider: Some("fake-reviewer".into()),
        model: None,
    };
    config.driver.max_parallel_components = 2;

    let implementation = SharedScopeWritingAgent;
    let reviewer = CleanReviewer;
    let agents = AgentSet {
        implementation: &implementation,
        reviewer: &reviewer,
        remediation: &reviewer,
    };
    let summary = with_working_directory(&repository, || {
        drive(
            &agents,
            &config,
            &paths,
            DriveWarrant {
                max_prds: 2,
                max_duration_ms: 600_000,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });

    assert_eq!(
        summary.completed, 2,
        "both independent, same-scope PRDs must still complete (termination: {:?})",
        summary.termination
    );
    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    let driver = DriverRepository::new(db.conn());
    let decisions = driver.selection_decisions(&summary.session_id).unwrap();
    assert!(
        decisions.iter().any(|(prd, decision, _)| prd == "PRD-2"
            && (decision == "deferred_scope_overlap" || decision == "deferred_scope_held")),
        "PRD-2 must defer on PRD-1's shared scope before its turn: {decisions:?}"
    );
    assert_eq!(
        decisions
            .iter()
            .filter(|(_, decision, _)| decision == "ready_selected")
            .count(),
        2
    );
}

// ---------------------------------------------------------------------
// AC4: worker routing includes a cheap-or-local task and an independent
// strong review. This exercises the exact pure routing function the
// driver composition root calls before every execution
// (`run::resolved_worker_plan`), so no subprocess or network is needed.
// ---------------------------------------------------------------------

#[test]
fn worker_routing_selects_a_cheap_local_worker_and_an_independent_strong_reviewer() {
    use familiar_ai_core::config::{
        RegistryWorkerConfig, WorkerCapabilityConfig, WorkerRegistryConfig, WorkerRouteRuleConfig,
    };
    use familiar_ai_core::AgentAdapterKind;

    let mut config = Config::default();
    config.review.enabled = true;

    let cheap_local = RegistryWorkerConfig {
        adapter: Some(AgentAdapterKind::Codex),
        provider: "local".into(),
        model: "phi-mini".into(),
        runtime: None,
        model_artifact: None,
        auth_profile: None,
        capability_profile: None,
        runtime_config: None,
        executable: None,
        capabilities: vec![
            WorkerCapabilityConfig::Implementation,
            WorkerCapabilityConfig::Remediation,
            WorkerCapabilityConfig::NarrowTask,
        ],
        fresh_process_isolation: true,
        context_tokens: 32_000,
        estimated_cost_microusd: Some(1),
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: Vec::new(),
    };
    let strong_reviewer = RegistryWorkerConfig {
        adapter: Some(AgentAdapterKind::Codex),
        provider: "cloud-strong".into(),
        model: "opus-review".into(),
        runtime: None,
        model_artifact: None,
        auth_profile: None,
        capability_profile: None,
        runtime_config: None,
        executable: None,
        capabilities: vec![WorkerCapabilityConfig::Review],
        fresh_process_isolation: true,
        context_tokens: 200_000,
        estimated_cost_microusd: Some(500_000),
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: Vec::new(),
    };

    let mut registry = WorkerRegistryConfig::default();
    registry.workers.insert("cheap-local".into(), cheap_local);
    registry
        .workers
        .insert("strong-reviewer".into(), strong_reviewer);
    registry.routing.rules = vec![WorkerRouteRuleConfig {
        id: "narrow-tasks-go-cheap".into(),
        worker: "cheap-local".into(),
        risk_classes: Vec::new(),
        max_expected_files: Some(5),
    }];
    config.worker_registry = Some(registry);

    let route_context = familiar_ai_daemon::run::RouteContext {
        risk_classes: Vec::new(),
        expected_file_count: 1,
    };
    let (implementation, reviewer, records) =
        familiar_ai_daemon::run::resolved_worker_plan(&config, &route_context).unwrap();

    assert_eq!(implementation.model.as_deref(), Some("phi-mini"));
    assert_eq!(reviewer.model.as_deref(), Some("opus-review"));
    assert_ne!(
        implementation.model, reviewer.model,
        "review must be independent of implementation"
    );
    assert_eq!(
        records.len(),
        3,
        "implementation, remediation, review all recorded"
    );
    assert!(records
        .iter()
        .any(|r| r.stage == familiar_ai_agent::WorkerStage::Review));
}

// ---------------------------------------------------------------------
// AC6: manual delivery stops at reviewed PR; explicit PoC self-approval
// self-approves only within its own bounded warrant and never targets
// production; review-gated automatic delivery preserves separation
// evidence (three distinct implementer/reviewer/approver identities).
// ---------------------------------------------------------------------

struct ScriptedRunner {
    calls: Mutex<Vec<Vec<String>>>,
}

impl ScriptedRunner {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, _directory: &Path, argv: &[String]) -> Result<Output, String> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let is_view = argv.iter().any(|value| value == "view");
        Ok(Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: if is_view {
                b"37\n".to_vec()
            } else {
                Vec::new()
            },
            stderr: Vec::new(),
        })
    }
}

fn delivery_fixture(mode: DeliveryMode) -> (tempfile::TempDir, PathBuf, DeliveryConfig) {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    let ownership = temp.path().join("attempt.ownership.json");
    fs::write(
        &ownership,
        serde_json::to_vec(&WorktreeOwnership {
            session_id: "session-acceptance".into(),
            prd_id: "PRD-038".into(),
            worktree,
            created_at: "2026-08-30T00:00:00Z".into(),
            heartbeat_at: "2026-08-30T00:00:00Z".into(),
            state: "ready_for_delivery".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let policy = DeliveryConfig {
        mode,
        enabled: true,
        max_deliveries_per_session: 1,
        command_timeout_ms: 1_000,
        remote: "origin".into(),
        base: "main".into(),
        provider_argv: vec!["provider".into()],
        staging_environment: "staging".into(),
        deploy_argv: vec!["deploy".into()],
        smoke_argv: vec!["smoke".into()],
        rollback_argv: vec!["rollback".into()],
        ..DeliveryConfig::default()
    };
    (temp, ownership, policy)
}

#[test]
fn manual_reviewed_pr_delivery_stops_before_merge_or_deploy() {
    let (_temp, ownership, policy) = delivery_fixture(DeliveryMode::ReviewedPrManual);
    let runner = ScriptedRunner::new();
    let delivered = deliver_with(&ownership, &policy, &runner).unwrap();
    assert_eq!(delivered.phase, "awaiting_merge_authority");
    assert_eq!(delivered.pr_number, Some(37));
    let calls = runner.calls.lock().unwrap();
    assert!(!calls.iter().any(|call| call.iter().any(|v| v == "merge")));
    assert!(!calls
        .iter()
        .any(|call| call.first().is_some_and(|v| v == "deploy")));
}

#[test]
fn poc_self_approval_delivers_within_its_warrant_and_never_targets_production() {
    let (_temp, ownership, mut policy) = delivery_fixture(DeliveryMode::PocSelfApproval);
    policy.poc_warrant = Some(PocSelfApprovalWarrant {
        actor: "human:acceptance".into(),
        max_prds: 1,
        expires_at: "2099-01-01T00:00:00Z".into(),
        assurance_label: "LOW_ASSURANCE_POC_SELF_APPROVAL".into(),
    });

    // Bounded by its warrant: max_deliveries_per_session may never exceed
    // warrant.max_prds.
    let mut over_bound = policy.clone();
    over_bound.max_deliveries_per_session = 2;
    assert!(over_bound
        .validate()
        .unwrap_err()
        .contains("cannot exceed the warrant max_prds"));

    // Never production, regardless of warrant validity.
    let mut production = policy.clone();
    production.staging_environment = "production".into();
    assert!(production
        .validate()
        .unwrap_err()
        .contains("prohibits production delivery"));

    // Within the warrant, self-approval proceeds unattended to completion
    // (no human gate blocks it, unlike reviewed-PR manual mode).
    policy.validate().unwrap();
    let runner = ScriptedRunner::new();
    let delivered = deliver_with(&ownership, &policy, &runner).unwrap();
    assert_eq!(delivered.phase, "staging_verified");
}

#[test]
fn review_gated_automatic_delivery_preserves_separation_evidence() {
    let (_temp, ownership, mut policy) = delivery_fixture(DeliveryMode::ReviewGatedAutomatic);
    policy.review_gate = Some(ReviewGateConfig {
        implementer: "worker:codex-implementer".into(),
        reviewer: "worker:claude-reviewer".into(),
        approver: "human:acceptance".into(),
    });
    policy.validate().unwrap();

    // Separation is a structural invariant, not merely a convention: any
    // collapsed pair is refused before any external effect.
    let mut collapsed = policy.clone();
    collapsed.review_gate = Some(ReviewGateConfig {
        implementer: "worker:codex-implementer".into(),
        reviewer: "worker:codex-implementer".into(),
        approver: "human:acceptance".into(),
    });
    assert!(collapsed.validate().unwrap_err().contains("three distinct"));

    let runner = ScriptedRunner::new();
    let delivered = deliver_with(&ownership, &policy, &runner).unwrap();
    assert_eq!(delivered.phase, "staging_verified");

    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let gate = policy.review_gate.as_ref().unwrap();
    let roles = [
        ("implementer_recorded", &gate.implementer),
        ("reviewer_recorded", &gate.reviewer),
        ("approver_recorded", &gate.approver),
    ];
    let repo = familiar_ai_storage::DeliveryRepository::new(db.conn());
    for (index, (decision, actor)) in roles.iter().enumerate() {
        repo.record_authority_decision(
            &format!("decision-{index}"),
            "/repo/.git",
            "session-acceptance",
            "PRD-038",
            "review_gated_automatic",
            actor,
            decision,
            None,
            "[]",
            "[]",
            None,
            0,
        )
        .unwrap();
    }
    let decisions = repo.decisions_for_session("session-acceptance").unwrap();
    let recorded_actors: BTreeSet<_> = decisions.iter().map(|d| d.actor.clone()).collect();
    assert_eq!(
        recorded_actors.len(),
        3,
        "durable evidence of three distinct actors"
    );
}

// ---------------------------------------------------------------------
// AC7: the morning report states work, blockers, cost, cache behavior,
// human gates, and recovery.
// ---------------------------------------------------------------------

#[test]
fn report_states_work_blockers_cost_cache_human_gates_and_recovery() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("docs/prds")).unwrap();
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(repository.join("src/lib.rs"), "// fixture\n").unwrap();
    fs::write(
        repository.join("docs/prds/PRD-001.md"),
        hand_written_prd(1, None, "src"),
    )
    .unwrap();
    git(&repository, &["init", "-q"]);
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "fixture"]);

    let paths = app_paths(temp.path());
    let config = base_config(&paths);
    struct RecordingAgent;
    impl CodingAgent for RecordingAgent {
        fn execute(
            &self,
            _request: ExecutionRequest<'_>,
            _output: &mut dyn std::io::Write,
        ) -> Result<ExecutionResult, AgentExecutionError> {
            Ok(ExecutionResult {
                exit_code: Some(0),
                ..ExecutionResult::default()
            })
        }
    }
    let agent = RecordingAgent;
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
                max_prds: 1,
                ..DriveWarrant::default()
            },
        )
        .unwrap()
    });

    let db = Database::open(config.database.path.as_ref().unwrap()).unwrap();
    // A human gate (a manual delivery authority decision) belongs to this
    // session too — the report's AUTHORITY DECISIONS section only renders
    // when at least one exists.
    familiar_ai_storage::DeliveryRepository::new(db.conn())
        .record_authority_decision(
            "decision-report-fixture",
            "/repo/.git",
            &summary.session_id,
            "PRD-001",
            "reviewed_pr_manual",
            "human:acceptance",
            "stopped_for_review",
            None,
            "[]",
            "[]",
            None,
            0,
        )
        .unwrap();
    let report = familiar_ai_daemon::report::render(&db, Some(&summary.session_id)).unwrap();
    for section in [
        "BUILT (",
        "STOPPED (",
        "AUTHORITY DECISIONS (",
        "RECOVERY",
        "COST",
        "CACHE",
        "NEEDS HUMAN JUDGMENT (",
    ] {
        assert!(
            report.contains(section),
            "missing {section:?} in:\n{report}"
        );
    }
}

// ---------------------------------------------------------------------
// AC8: repeating the proof creates no duplicate claims, PRs, merges, or
// deployments.
// ---------------------------------------------------------------------

#[test]
fn repeating_a_completed_delivery_never_repeats_pr_merge_or_deploy_effects() {
    let (_temp, ownership, policy) = delivery_fixture(DeliveryMode::ReviewGatedAutomatic);
    let policy = DeliveryConfig {
        review_gate: Some(ReviewGateConfig {
            implementer: "worker:codex-implementer".into(),
            reviewer: "worker:claude-reviewer".into(),
            approver: "human:acceptance".into(),
        }),
        ..policy
    };
    let runner = ScriptedRunner::new();
    let first = deliver_with(&ownership, &policy, &runner).unwrap();
    assert_eq!(first.phase, "staging_verified");
    let second = deliver_with(&ownership, &policy, &runner).unwrap();
    assert_eq!(second.phase, "staging_verified");

    let calls = runner.calls.lock().unwrap();
    for command in ["commit", "push", "create", "merge"] {
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.iter().any(|value| value == command))
                .count(),
            1,
            "repeating delivery must not repeat {command}"
        );
    }
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.first().is_some_and(|v| v == "deploy"))
            .count(),
        1,
        "repeating delivery must not repeat deploy"
    );
}

#[test]
fn repeating_batch_approval_creates_no_duplicate_planner_record() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("docs/prds/proposed/batch-9038")).unwrap();
    git(&repository, &["init", "-q"]);
    fs::write(
        repository.join("docs/prds/proposed/batch-9038/PRD-001.md"),
        drafted_prd(1, None, "component-a"),
    )
    .unwrap();
    let identity = FilesystemBacklogDiscovery.resolve(&repository).unwrap();
    let mut db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let limits = PlannerConfig {
        agent: Default::default(),
        max_prds_per_batch: 8,
        max_bytes_per_prd: 65536,
    };
    plan::approve(
        &repository,
        "batch-9038",
        "human:acceptance",
        &limits,
        &identity,
        &mut db,
    )
    .unwrap();
    // Repeating approval of the same batch id finds no proposal left to
    // approve — it fails rather than silently re-claiming or duplicating.
    let repeat = plan::approve(
        &repository,
        "batch-9038",
        "human:acceptance",
        &limits,
        &identity,
        &mut db,
    );
    assert!(repeat.is_err());
    let rows: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM planner_batches WHERE batch_id='batch-9038'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1, "no duplicate approval record");
}
