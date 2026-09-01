//! PRD-038: Multi-Repository Have-At-It Acceptance — MCP surface.
//!
//! Complements `familiar-ai-daemon`'s `multi_repo_acceptance.rs`. This file
//! proves that the context/stewardship surface a coding agent actually talks
//! to (a) behaves identically across materially different repositories and
//! (b) that the model-facing control-plane boundary can report progress and
//! request escalation but can never approve its own work — the MCP-side half
//! of "independent strong review" and repeated-call idempotency.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tempfile::tempdir;

use familiar_ai_core::config::Config;
use familiar_ai_core::AppStatus;
use familiar_ai_core::{
    BacklogDiscovery, BacklogStatusStore, DiscoveredPrd, FilesystemBacklogDiscovery,
    RepositoryIdentity,
};
use familiar_ai_mcp::storage::SqliteStorage;
use familiar_ai_mcp::tool::{Tool, ToolContext};
use familiar_ai_mcp::tools::stewardship_mutations::BacklogCompleteTool;
use familiar_ai_mcp::tools::stewardship_reads::ListBacklogTool;
use familiar_ai_storage::{Database, SqliteBacklogRepository};

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
}

/// A materially different checked-in fixture repository (real Cargo/npm
/// content, not synthetic strings), given two PRDs and reconciled into a
/// fresh database — the same fixture shape `stewardship.rs` uses for the
/// single-repository suite, generalized to any fixture tree.
fn repo_fixture(
    fixture_name: &str,
) -> (
    tempfile::TempDir,
    PathBuf,
    RepositoryIdentity,
    Vec<DiscoveredPrd>,
) {
    let repo = tempdir().unwrap();
    copy_fixture_repository(fixture_name, repo.path());
    fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
    fs::write(repo.path().join("docs/prds/PRD-1.md"), "# PRD-1: One\n").unwrap();
    fs::write(repo.path().join("docs/prds/PRD-2.md"), "# PRD-2: Two\n").unwrap();
    git(repo.path(), &["init", "-q"]);
    // A container or CI runner may have no global git identity; the drive
    // commits during integration, so the fixture provides its own.
    git(
        repo.path(),
        &["config", "user.email", "fixture@familiar-ai.invalid"],
    );
    git(repo.path(), &["config", "user.name", "fixture"]);
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "fixture"]);

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let discovered = FilesystemBacklogDiscovery.discover(&identity).unwrap();

    let database = repo.path().join("state.db");
    let mut db = Database::open(&database).unwrap();
    db.run_migrations().unwrap();
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(&identity, &discovered)
        .unwrap();
    drop(db);

    (repo, database, identity, discovered)
}

fn ctx_for(database: &Path) -> ToolContext {
    let db = Arc::new(Mutex::new(Database::open(database).unwrap()));
    ToolContext {
        storage: Arc::new(SqliteStorage::new(db)),
        status: Arc::new(Mutex::new(AppStatus::new())),
        config: Arc::new(Config::default()),
        router: None,
    }
}

// ---------------------------------------------------------------------
// AC1: the stewardship/context tool surface behaves identically across
// materially different repositories (language, dependency manifest, and
// directory layout differ; the MCP tool code does not).
// ---------------------------------------------------------------------

async fn assert_backlog_surface_is_repository_shape_agnostic(fixture_name: &str) {
    let (repo, database, _identity, _discovered) = repo_fixture(fixture_name);
    let ctx = ctx_for(&database);

    let result = ListBacklogTool
        .call(
            json!({"repository_path": repo.path().to_str().unwrap()}),
            &ctx,
        )
        .await
        .unwrap();
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "fixture {fixture_name}");
    assert_eq!(items[0]["prd_path"], "docs/prds/PRD-1.md");
    assert_eq!(items[0]["status"], "pending");
    assert_eq!(items[1]["prd_path"], "docs/prds/PRD-2.md");
    assert_eq!(items[1]["status"], "pending");

    // A directory outside this repository never sees its backlog, regardless
    // of the repository's own language or layout.
    let other = tempdir().unwrap();
    let outside = ListBacklogTool
        .call(
            json!({"repository_path": other.path().to_str().unwrap()}),
            &ctx,
        )
        .await;
    assert!(outside.is_err(), "fixture {fixture_name}");
}

#[tokio::test]
async fn rust_cargo_repository_backlog_surface_matches_the_generic_shape() {
    assert_backlog_surface_is_repository_shape_agnostic("repo-rust-cli").await;
}

#[tokio::test]
async fn node_npm_repository_backlog_surface_matches_the_generic_shape() {
    assert_backlog_surface_is_repository_shape_agnostic("repo-node-service").await;
}

// ---------------------------------------------------------------------
// AC4 / AC6: the model-facing control-plane surface can report progress
// and request escalation, but structurally has no approval tool — the
// MCP-side evidence that review/approval authority is independent of the
// worker being reviewed.
// ---------------------------------------------------------------------

#[tokio::test]
async fn control_plane_worker_surface_can_progress_and_escalate_but_never_approve() {
    use familiar_ai_core::control_plane::{
        Authority, CapabilityScope, ClientClass, ExecutionMode, SchedulingPolicy, Submission,
        CONTROL_PROTOCOL_VERSION,
    };
    use familiar_ai_daemon::{
        control_plane::ControlPlaneService,
        local_transport::{ClientHello, LocalClient, LocalHost},
    };
    use familiar_ai_mcp::storage::UnavailableStorage;
    use familiar_ai_mcp::tool::ToolRegistry;
    use familiar_ai_mcp::tools::control_plane;

    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("socket");
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let service =
        ControlPlaneService::new(Arc::new(Mutex::new(db)), SchedulingPolicy::default(), 1);
    service
        .register_project("acceptance", temp.path().to_str().unwrap(), 0, None)
        .unwrap();
    let operator = CapabilityScope {
        client_class: ClientClass::Operator,
        project_id: Some("acceptance".into()),
        execution_id: None,
        attempt: None,
        worker_id: None,
        authorities: vec![Authority::Control],
    };
    service
        .submit(
            &operator,
            &Submission {
                execution_id: "execution-acceptance".into(),
                project_id: "acceptance".into(),
                idempotency_key: "key-acceptance".into(),
                mode: ExecutionMode::Detached,
                priority: 0,
                command_json: "[]".into(),
            },
        )
        .unwrap();
    service.claim_next().unwrap();
    service
        .bind_worker(
            "execution-acceptance",
            "worker-acceptance",
            std::process::id(),
            "test",
            "hash",
        )
        .unwrap();
    let grant = service
        .mint_worker_session("execution-acceptance", "worker-acceptance")
        .unwrap();
    let _host = LocalHost::bind(&socket, "nonce-acceptance".into(), service)
        .await
        .unwrap();
    let client = LocalClient::connect(
        &socket,
        ClientHello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "acceptance".into(),
            session_reference: Some(grant.credential),
            owner_nonce: Some("nonce-acceptance".into()),
        },
    )
    .await
    .unwrap();

    let mut registry = ToolRegistry::new();
    control_plane::register(&mut registry, Arc::new(tokio::sync::Mutex::new(client)));
    let ctx = ToolContext {
        storage: Arc::new(UnavailableStorage),
        status: Arc::new(Mutex::new(AppStatus::new())),
        config: Arc::new(Config::default()),
        router: None,
    };

    // The worker can report its own progress...
    assert!(registry
        .call(
            "control.report_progress",
            json!({"payload":{"stage":"implementation"}}),
            &ctx,
        )
        .await
        .is_ok());
    // ...and can request escalation, which only ever creates a pending human
    // gate, never an approval.
    let gate = registry
        .call(
            "control.request_escalation",
            json!({"payload":{"capability":"network"}}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(gate["state"], "pending_human");
    // No tool on this surface can approve anything — review/approval
    // authority is structurally independent of the worker being reviewed.
    assert!(!registry.list().iter().any(|t| t.name.contains("approve")));
}

// ---------------------------------------------------------------------
// AC8: repeating a stewardship completion call through MCP creates no
// duplicate claim or completion.
// ---------------------------------------------------------------------

#[tokio::test]
async fn repeating_backlog_completion_through_mcp_creates_no_duplicate_completion() {
    let (repo, database, identity, discovered) = repo_fixture("repo-rust-cli");
    {
        let mut db = Database::open(&database).unwrap();
        SqliteBacklogRepository::new(db.conn_mut())
            .claim_run(
                &identity,
                &discovered,
                &discovered[0],
                "system:familiar-ai-run:00000000000000000001-0000000001-000001",
            )
            .unwrap();
    }
    let ctx = ctx_for(&database);

    let first = BacklogCompleteTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "prd_path": "docs/prds/PRD-1.md",
                "actor": "human:acceptance",
                "reason": "verified manually",
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(first["new_status"], "completed");

    // Repeating the exact same call must not silently record a second
    // completion of already-completed work.
    let repeat = BacklogCompleteTool
        .call(
            json!({
                "repository_path": repo.path().to_str().unwrap(),
                "prd_path": "docs/prds/PRD-1.md",
                "actor": "human:acceptance",
                "reason": "verified manually",
            }),
            &ctx,
        )
        .await;
    assert!(
        repeat.is_err(),
        "repeated completion must be refused, not duplicated"
    );

    let db = Database::open(&database).unwrap();
    let completions: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM backlog_status_events WHERE prd_path='docs/prds/PRD-1.md' AND new_status='completed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(completions, 1, "no duplicate completion event");
}
