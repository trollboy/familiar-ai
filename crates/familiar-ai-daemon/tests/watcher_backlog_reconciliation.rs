//! PRD-108: the daemon's own backlog reconciler stays in sync with the PRD
//! filesystem, from a real temporary Git repository, a real `FileWatcher`,
//! and the exact library-level path `main.rs`'s watcher handler takes
//! (`BacklogReconciler::observe_event` fed from a `WatcherEvent` channel).
//!
//! `main.rs` itself is a binary target and cannot be exercised from an
//! integration test; every scenario here drives the same `familiar-ai-daemon`
//! library code `main.rs` wires together, which is where all of PRD-108's
//! behavior actually lives.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use familiar_ai_core::operator_ui::{OperatorAction, OperatorDataSource, OperatorQuery};
use familiar_ai_core::{BacklogDiscovery, Config, FilesystemBacklogDiscovery};
use familiar_ai_daemon::backlog_reconciler::BacklogReconciler;
use familiar_ai_daemon::operator_ui::OperatorDispatcher;
use familiar_ai_storage::{list_backlog_entries, Database};
use familiar_ai_watcher::{FileWatcher, WatcherEvent};
use tokio::sync::mpsc;

struct NoopSource;
impl OperatorDataSource for NoopSource {
    fn query(&self, _: OperatorQuery) -> Result<serde_json::Value, String> {
        Err("unused in this test".into())
    }
    fn act(&self, _: OperatorAction) -> Result<serde_json::Value, String> {
        Err("unused in this test".into())
    }
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .status()
            .unwrap()
            .success(),
        "git {args:?}"
    );
}

fn write_prd(repo: &Path, relative: &str, number: u32, title: &str) {
    let path = repo.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!("# PRD-{number}: {title}\n\n**Depends on:** none\n"),
    )
    .unwrap();
}

/// A temporary Git repository with one active PRD, committed so discovery
/// and `git rev-parse` both resolve cleanly.
fn temp_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    write_prd(repo.path(), "docs/prds/PRD-001.md", 1, "One");
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "base"]);
    repo
}

fn config_for(repo: &Path) -> Config {
    let mut config = Config::default();
    config
        .repositories
        .insert(repo.display().to_string(), Default::default());
    config
}

fn open_db() -> Arc<Mutex<Database>> {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    Arc::new(Mutex::new(db))
}

fn entries(
    db: &Arc<Mutex<Database>>,
    identity: &familiar_ai_core::RepositoryIdentity,
) -> Vec<familiar_ai_storage::BacklogEntryRow> {
    list_backlog_entries(db.lock().unwrap().conn(), &identity.key, None, None, 100).unwrap()
}

/// The exact wiring `main.rs`'s watcher handler performs: drain real
/// `WatcherEvent`s and hand each one to the reconciler.
fn spawn_event_forwarder(
    reconciler: Arc<BacklogReconciler>,
    mut rx: mpsc::Receiver<WatcherEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            reconciler.observe_event(&event);
        }
    })
}

/// Startup: a repository with a pre-existing backlog is fully discovered
/// and committed before any watcher event or CLI command runs.
#[tokio::test(flavor = "multi_thread")]
async fn startup_enrolls_a_preexisting_backlog() {
    let repo = temp_repo();
    let db = open_db();
    let reconciler = BacklogReconciler::new(
        db.clone(),
        config_for(repo.path()),
        Duration::from_millis(30),
        Duration::from_secs(3600),
    );
    reconciler.reconcile_all_configured();

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let rows = entries(&db, &identity);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].prd_path, "docs/prds/PRD-001.md");
    assert_eq!(rows[0].status, "pending");
}

/// A real filesystem watcher observes a newly created PRD file and the
/// debounced reconciler discovers it, without any `next`/`run`/`drive`.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_active_prd_created_under_watch_is_discovered() {
    let repo = temp_repo();
    let db = open_db();
    let reconciler = BacklogReconciler::new(
        db.clone(),
        config_for(repo.path()),
        Duration::from_millis(100),
        Duration::from_secs(3600),
    );
    reconciler.reconcile_all_configured();

    let watcher_config = familiar_ai_core::config::WatcherConfig {
        enabled: true,
        paths: vec![repo.path().to_path_buf()],
        debounce_ms: 50,
        ignore_patterns: vec![],
        respect_gitignore: false,
    };
    let (tx, rx) = mpsc::channel(256);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let watcher = FileWatcher::new(watcher_config);
    let watcher_task = tokio::spawn(async move { watcher.run(tx, shutdown_rx).await });
    let forwarder = spawn_event_forwarder(reconciler.clone(), rx);

    tokio::time::sleep(Duration::from_millis(300)).await;
    write_prd(repo.path(), "docs/prds/PRD-002.md", 2, "Two");

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if entries(&db, &identity).len() == 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "watcher-triggered reconciliation did not discover the new PRD in time"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), watcher_task).await;
    forwarder.abort();
}

/// Metadata modification (front-matter status change) reaches the ledger
/// through the same debounced path as a create.
#[tokio::test(flavor = "multi_thread")]
async fn modifying_prd_metadata_updates_the_discovered_content_hash() {
    let repo = temp_repo();
    let db = open_db();
    let reconciler = BacklogReconciler::new(
        db.clone(),
        config_for(repo.path()),
        Duration::from_millis(20),
        Duration::from_secs(3600),
    );
    reconciler.reconcile_now(repo.path()).unwrap();
    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let before = entries(&db, &identity)[0].clone();

    fs::write(
        repo.path().join("docs/prds/PRD-001.md"),
        "# PRD-1: One (revised)\n\n**Depends on:** none\n",
    )
    .unwrap();
    reconciler.observe_event(&WatcherEvent::FileChanged {
        path: repo.path().join("docs/prds/PRD-001.md"),
        repo_root: Some(repo.path().to_path_buf()),
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let after = entries(&db, &identity)[0].clone();
    assert_ne!(before.discovered_at, "");
    assert_ne!(before.updated_at, after.updated_at);
}

/// Deleting a PRD (not moving it to `done/`) leaves the row behind marked
/// missing rather than deleted — the existing immutable-history contract —
/// once the reconciler observes the removal.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_prd_marks_the_row_missing_without_deleting_history() {
    let repo = temp_repo();
    let db = open_db();
    let reconciler = BacklogReconciler::new(
        db.clone(),
        config_for(repo.path()),
        Duration::from_millis(20),
        Duration::from_secs(3600),
    );
    reconciler.reconcile_now(repo.path()).unwrap();

    fs::remove_file(repo.path().join("docs/prds/PRD-001.md")).unwrap();
    reconciler.observe_event(&WatcherEvent::FileRemoved {
        path: repo.path().join("docs/prds/PRD-001.md"),
        repo_root: Some(repo.path().to_path_buf()),
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let rows = entries(&db, &identity);
    assert_eq!(rows.len(), 1, "the row survives, it is not deleted");
    assert!(rows[0].missing_since.is_some());
}

/// Moving a PRD from active into `done/` produces a current-path completed
/// row while the old path's history is preserved, not counted as current
/// missing work.
#[tokio::test(flavor = "multi_thread")]
async fn active_to_done_rename_produces_a_completed_row_and_preserves_history() {
    let repo = temp_repo();
    let db = open_db();
    let reconciler = BacklogReconciler::new(
        db.clone(),
        config_for(repo.path()),
        Duration::from_millis(20),
        Duration::from_secs(3600),
    );
    reconciler.reconcile_now(repo.path()).unwrap();

    fs::create_dir_all(repo.path().join("docs/prds/done")).unwrap();
    let old_path = repo.path().join("docs/prds/PRD-001.md");
    let new_path = repo.path().join("docs/prds/done/PRD-001.md");
    fs::rename(&old_path, &new_path).unwrap();
    reconciler.observe_event(&WatcherEvent::FileRenamed {
        old_path,
        new_path,
        repo_root: Some(repo.path().to_path_buf()),
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let rows = entries(&db, &identity);
    let done = rows
        .iter()
        .find(|r| r.prd_path == "docs/prds/done/PRD-001.md")
        .expect("the new path is discovered as completed");
    assert_eq!(done.status, "completed");
    assert!(done.missing_since.is_none());
    let old = rows
        .iter()
        .find(|r| r.prd_path == "docs/prds/PRD-001.md")
        .expect("the old path's row survives as history");
    assert!(old.missing_since.is_some());
}

/// Several rapid events for the same repository collapse into exactly one
/// reconciliation and exactly one operator refresh event.
#[tokio::test(flavor = "multi_thread")]
async fn a_burst_of_events_collapses_into_one_reconciliation_and_one_event() {
    let repo = temp_repo();
    let db = open_db();
    let reconciler = BacklogReconciler::new(
        db,
        config_for(repo.path()),
        Duration::from_millis(150),
        Duration::from_secs(3600),
    );
    let dispatcher = Arc::new(OperatorDispatcher::new(Arc::new(NoopSource), 1));
    reconciler.set_event_sink(dispatcher.clone());

    let path = repo.path().join("docs/prds/PRD-001.md");
    for _ in 0..8 {
        reconciler.observe_event(&WatcherEvent::FileChanged {
            path: path.clone(),
            repo_root: Some(repo.path().to_path_buf()),
        });
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    let events = dispatcher.observe(0, 20).unwrap();
    assert_eq!(
        events.len(),
        1,
        "a burst must collapse into one reconciliation"
    );
    // The desktop's `heartbeat()` (`ui/app.js`) polls `operator_observe` and
    // refreshes on any event whose sequence is contiguous with what it last
    // saw — unchanged by PRD-108. This event needs no special handling on
    // the client: it moves the UI from an old snapshot to the reconciled
    // one exactly like a mutation-caused event would.
    assert_eq!(events[0].sequence, 1);
}

/// An unrelated file change (outside the configured PRD locations) never
/// schedules a reconciliation.
#[tokio::test(flavor = "multi_thread")]
async fn unrelated_file_events_do_nothing() {
    let repo = temp_repo();
    let db = open_db();
    let reconciler = BacklogReconciler::new(
        db,
        config_for(repo.path()),
        Duration::from_millis(20),
        Duration::from_secs(3600),
    );
    let dispatcher = Arc::new(OperatorDispatcher::new(Arc::new(NoopSource), 1));
    reconciler.set_event_sink(dispatcher.clone());

    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/main.rs"), "fn main() {}").unwrap();
    reconciler.observe_event(&WatcherEvent::FileCreated {
        path: repo.path().join("src/main.rs"),
        repo_root: Some(repo.path().to_path_buf()),
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(dispatcher.observe(0, 10).unwrap().is_empty());
}

/// Watcher downtime: a change lands on disk while nothing is observing it.
/// The next bounded read-time reconciliation repairs the gap.
#[tokio::test(flavor = "multi_thread")]
async fn watcher_downtime_is_repaired_by_the_bounded_read_fallback() {
    let repo = temp_repo();
    let db = open_db();
    let reconciler = BacklogReconciler::new(
        db.clone(),
        config_for(repo.path()),
        Duration::from_millis(20),
        Duration::from_millis(30),
    );
    reconciler.reconcile_now(repo.path()).unwrap();

    // No watcher runs here — this simulates the gap.
    write_prd(repo.path(), "docs/prds/PRD-002.md", 2, "Two");

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    assert_eq!(
        entries(&db, &identity).len(),
        1,
        "nothing observed the change yet"
    );

    tokio::time::sleep(Duration::from_millis(50)).await;
    reconciler.reconcile_if_stale(repo.path());
    assert_eq!(
        entries(&db, &identity).len(),
        2,
        "the read repaired the gap"
    );
}

/// The bounded read fallback must not become a self-sustaining refresh
/// loop. A client query triggers `reconcile_if_stale`; if a reconciliation
/// that changes nothing still published an operator event, that event would
/// cause the client to refresh, re-issue the query, and trigger another
/// reconciliation once the bound elapses — continuous, not bounded. Polling
/// an unchanged repository repeatedly must publish no more events than the
/// one reconciliation that actually discovered its state.
#[tokio::test(flavor = "multi_thread")]
async fn repeated_read_fallback_reconciliation_of_an_unchanged_repository_emits_no_events() {
    let repo = temp_repo();
    let db = open_db();
    let reconciler = BacklogReconciler::new(
        db.clone(),
        config_for(repo.path()),
        Duration::from_millis(20),
        Duration::from_millis(5),
    );
    let dispatcher = Arc::new(OperatorDispatcher::new(Arc::new(NoopSource), 1));
    reconciler.set_event_sink(dispatcher.clone());

    // The first reconciliation of this repository in the process has no
    // prior fingerprint to compare against, so it always publishes.
    reconciler.reconcile_now(repo.path()).unwrap();
    let after_first = dispatcher.observe(0, 20).unwrap().len();
    assert_eq!(after_first, 1);

    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        reconciler.reconcile_if_stale(repo.path());
    }

    let events = dispatcher.observe(0, 20).unwrap();
    assert_eq!(
        events.len(),
        after_first,
        "reconciling an unchanged repository through the read fallback must not emit further refresh events"
    );
}

/// A discovery failure (malformed PRD) preserves the last valid snapshot and
/// records a repository-scoped diagnostic instead of fabricating an empty
/// backlog.
#[test]
fn a_failed_reconciliation_preserves_the_prior_snapshot_and_records_a_diagnostic() {
    let repo = temp_repo();
    let db = open_db();
    let reconciler = BacklogReconciler::new(
        db.clone(),
        config_for(repo.path()),
        Duration::from_millis(20),
        Duration::from_secs(3600),
    );
    reconciler.reconcile_now(repo.path()).unwrap();
    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    assert!(reconciler.diagnostic(&identity.key).is_none());

    fs::write(repo.path().join("docs/prds/PRD-001.md"), "not a prd at all").unwrap();
    assert!(reconciler.reconcile_now(repo.path()).is_err());
    assert!(reconciler.diagnostic(&identity.key).is_some());

    let rows = entries(&db, &identity);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "pending");
}
