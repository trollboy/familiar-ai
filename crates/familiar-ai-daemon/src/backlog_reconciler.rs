//! PRD-108: the daemon's single backlog reconciler.
//!
//! Filesystem discovery and transactional reconciliation
//! ([`BacklogStatusStore::reconcile_and_snapshot`]) already exist and are
//! already what `next`, `run`, `drive`, and `resume` use — but only when one
//! of those commands runs. Nothing in the long-running daemon ever called
//! it, so the backlog stayed whatever the last command left it as: a newly
//! pulled PRD had no ledger row, and a PRD moved into `docs/prds/done/`
//! could sit next to a stale row at its old path indefinitely.
//!
//! This module makes the daemon itself keep the backlog synchronized, from
//! three call sites: daemon startup (before anything can be queried),
//! watcher events (debounced, coalesced per repository), and a bounded
//! reconcile-on-read fallback for operator queries. The watcher is a hint,
//! never authoritative — every reconciliation re-discovers the full
//! configured PRD locations and commits one complete view through the same
//! `reconcile_and_snapshot` the CLI already trusts.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use familiar_ai_core::{BacklogDiscovery, BacklogStatusStore, Config, FilesystemBacklogDiscovery};
use familiar_ai_storage::{Database, SqliteBacklogRepository};
use familiar_ai_watcher::WatcherEvent;

use crate::operator_ui::OperatorDispatcher;

/// Per-repository debounce state. At most one reconciliation runs and at
/// most one rerun is queued behind it; further schedule requests while
/// either is true are no-ops, which is what keeps a burst of watcher events
/// from creating an unbounded task or queue.
#[derive(Default)]
struct RepoDebounce {
    /// A debounce timer is pending; it has not fired yet.
    scheduled: bool,
    /// A reconciliation is currently running.
    running: bool,
    /// An event arrived while `running`; run exactly once more after.
    rerun: bool,
}

pub struct BacklogReconciler {
    db: Arc<Mutex<Database>>,
    config: Config,
    debounce: Duration,
    read_fallback_interval: Duration,
    events: Mutex<Option<Arc<OperatorDispatcher>>>,
    debounce_state: Mutex<HashMap<String, Arc<Mutex<RepoDebounce>>>>,
    freshness: Mutex<HashMap<String, Arc<Mutex<Instant>>>>,
    diagnostics: Mutex<HashMap<String, String>>,
    /// The last committed backlog's fingerprint per repository (see
    /// [`Self::backlog_fingerprint`]), used to suppress publishing an
    /// operator event when a reconciliation changes nothing. Without this,
    /// the bounded reconcile-on-read fallback (`reconcile_if_stale`) would
    /// still emit an event on every no-op pass; a connected client refreshes
    /// on that event, re-issues the query that triggered the fallback, and
    /// the next bounded interval fires again — a self-sustaining loop rather
    /// than the bounded fallback the PRD requires.
    fingerprints: Mutex<HashMap<String, u64>>,
}

impl BacklogReconciler {
    pub fn new(
        db: Arc<Mutex<Database>>,
        config: Config,
        debounce: Duration,
        read_fallback_interval: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            config,
            debounce,
            read_fallback_interval,
            events: Mutex::new(None),
            debounce_state: Mutex::new(HashMap::new()),
            freshness: Mutex::new(HashMap::new()),
            diagnostics: Mutex::new(HashMap::new()),
            fingerprints: Mutex::new(HashMap::new()),
        })
    }

    /// Wired in once the daemon has constructed its `OperatorDispatcher`.
    /// Startup reconciliation (see [`Self::reconcile_all_configured`]) runs
    /// before that dispatcher exists and before anything can query it, so it
    /// never needs to publish an event; every reconciliation after that does.
    pub fn set_event_sink(&self, sink: Arc<OperatorDispatcher>) {
        *self.events.lock().unwrap() = Some(sink);
    }

    /// Reconcile every configured repository. Called once at daemon startup,
    /// before the control socket is bound or the dashboard is spawned, so no
    /// client can ever observe a pre-reconciliation snapshot.
    pub fn reconcile_all_configured(&self) {
        for worktree in self.config.repositories.keys() {
            if let Err(error) = self.reconcile_now(Path::new(worktree)) {
                tracing::error!(repository = %worktree, %error, "startup backlog reconciliation failed");
            }
        }
    }

    /// The core synchronous reconcile-and-commit for one repository
    /// worktree. Discovery (filesystem I/O) happens before the database is
    /// locked; the resulting snapshot commits atomically through the
    /// existing `reconcile_and_snapshot`. A discovery or transaction failure
    /// leaves the prior snapshot untouched and records a repository-scoped
    /// diagnostic instead of fabricating an empty backlog.
    pub fn reconcile_now(&self, worktree: &Path) -> Result<(), String> {
        let repository = FilesystemBacklogDiscovery
            .resolve(worktree)
            .map_err(|e| e.to_string())?;
        let repository_config = self
            .config
            .repository(&repository.worktree)
            .map_err(|e| e.to_string())?;
        let layout = repository_config.layout();
        let discovered = match FilesystemBacklogDiscovery.discover_with_layout(&repository, &layout)
        {
            Ok(discovered) => discovered,
            Err(error) => {
                let message = error.to_string();
                self.record_diagnostic(&repository.key, &message);
                return Err(message);
            }
        };
        {
            let mut db = self
                .db
                .lock()
                .map_err(|_| "database lock poisoned".to_string())?;
            if let Err(error) = SqliteBacklogRepository::new(db.conn_mut())
                .reconcile_and_snapshot(&repository, &discovered)
            {
                let message = error.to_string();
                self.record_diagnostic(&repository.key, &message);
                return Err(message);
            }
        }
        self.clear_diagnostic(&repository.key);
        self.touch_freshness(&repository.key);
        if self.backlog_changed(&repository.key) {
            if let Some(sink) = self.events.lock().unwrap().clone() {
                let _ = sink.record_event(format!("backlog_reconciled:{}", repository.key));
            }
        }
        Ok(())
    }

    /// Whether the backlog just committed for `repository_key` differs from
    /// what was committed the previous time this repository was reconciled,
    /// and records the new fingerprint either way. `reconcile_and_snapshot`
    /// bumps `last_seen_at` (always) and `updated_at` (for every present row)
    /// on every pass regardless of whether anything meaningful changed, so
    /// the fingerprint is built only from the fields that identify a row's
    /// actual state — path, number, suffix, status, and whether/when it went
    /// missing — not those timestamps. A repository reconciled for the first
    /// time in this process (no prior fingerprint) counts as changed, so
    /// startup and the first watcher/read-triggered reconciliation still
    /// publish.
    fn backlog_changed(&self, repository_key: &str) -> bool {
        let fingerprint = match self.backlog_fingerprint(repository_key) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                tracing::warn!(repository = %repository_key, %error, "could not compute backlog fingerprint; publishing a refresh event defensively");
                return true;
            }
        };
        let mut fingerprints = self.fingerprints.lock().unwrap();
        fingerprints.insert(repository_key.to_string(), fingerprint) != Some(fingerprint)
    }

    /// A stable hash of `repository_key`'s current backlog, over the fields
    /// that identify each row's actual state rather than the timestamps that
    /// change on every reconciliation pass regardless of content.
    fn backlog_fingerprint(&self, repository_key: &str) -> Result<u64, String> {
        let db = self
            .db
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let mut hasher = DefaultHasher::new();
        let mut after: Option<String> = None;
        const PAGE_SIZE: usize = 500;
        loop {
            let page = familiar_ai_storage::list_backlog_entries(
                db.conn(),
                repository_key,
                None,
                after.as_deref(),
                PAGE_SIZE,
            )
            .map_err(|e| e.to_string())?;
            let page_len = page.len();
            for entry in &page {
                entry.prd_path.hash(&mut hasher);
                entry.prd_number.hash(&mut hasher);
                entry.prd_suffix.hash(&mut hasher);
                entry.status.hash(&mut hasher);
                entry.missing_since.hash(&mut hasher);
            }
            after = page.last().map(|entry| entry.prd_path.clone());
            if page_len < PAGE_SIZE {
                break;
            }
        }
        Ok(hasher.finish())
    }

    /// Whether a changed path under `repo_root` falls under that
    /// repository's configured active or archived PRD locations. An
    /// ordinary source-code edit resolves false; nothing under
    /// `docs/prds/**` (or the repository's configured equivalent) is missed.
    pub fn is_relevant(&self, repo_root: &Path, changed_path: &Path) -> bool {
        let Ok(identity) = FilesystemBacklogDiscovery.resolve(repo_root) else {
            return false;
        };
        let Ok(repository_config) = self.config.repository(&identity.worktree) else {
            return false;
        };
        let layout = repository_config.layout();
        let active = identity.worktree.join(layout.active_dir.as_str());
        let archived = identity.worktree.join(layout.archived_dir.as_str());
        changed_path.starts_with(&active) || changed_path.starts_with(&archived)
    }

    /// Inspects one watcher event and schedules a debounced reconciliation
    /// when any path it names falls under a PRD location. A rename arriving
    /// as two events, one event, or an ambiguous event all reach here the
    /// same way — every reconciliation rescans the whole layout, so which
    /// shape the rename took does not change the result. An ambiguous event
    /// schedules whenever any of its paths are relevant, rather than being
    /// silently dropped and leaving the ledger knowingly stale.
    pub fn observe_event(self: &Arc<Self>, event: &WatcherEvent) {
        let (repo_root, paths): (Option<&Path>, Vec<&Path>) = match event {
            WatcherEvent::FileCreated { path, repo_root }
            | WatcherEvent::FileChanged { path, repo_root }
            | WatcherEvent::FileRemoved { path, repo_root } => {
                (repo_root.as_deref(), vec![path.as_path()])
            }
            WatcherEvent::FileRenamed {
                old_path,
                new_path,
                repo_root,
            } => (
                repo_root.as_deref(),
                vec![old_path.as_path(), new_path.as_path()],
            ),
            WatcherEvent::FileAmbiguous {
                paths, repo_root, ..
            } => (
                repo_root.as_deref(),
                paths.iter().map(PathBuf::as_path).collect(),
            ),
            WatcherEvent::RepoDiscovered { .. } | WatcherEvent::WatchError { .. } => return,
        };
        let Some(repo_root) = repo_root else {
            return;
        };
        if paths.iter().any(|path| self.is_relevant(repo_root, path)) {
            self.schedule(repo_root.to_path_buf());
        }
    }

    /// Debounced scheduling for a watcher-observed change under `worktree`.
    /// Coalesces bursts: while a timer is already pending, a further
    /// request is a no-op; while a reconciliation is already running, a
    /// further request sets exactly one queued rerun.
    pub fn schedule(self: &Arc<Self>, worktree: PathBuf) {
        let key = match FilesystemBacklogDiscovery.resolve(&worktree) {
            Ok(identity) => identity.key,
            Err(_) => return,
        };
        let entry = {
            let mut map = self.debounce_state.lock().unwrap();
            map.entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(RepoDebounce::default())))
                .clone()
        };
        {
            let mut state = entry.lock().unwrap();
            if state.running {
                state.rerun = true;
                return;
            }
            if state.scheduled {
                return;
            }
            state.scheduled = true;
        }
        let this = self.clone();
        let debounce = self.debounce;
        tokio::spawn(async move {
            tokio::time::sleep(debounce).await;
            this.fire(key, worktree, entry).await;
        });
    }

    async fn fire(
        self: Arc<Self>,
        key: String,
        worktree: PathBuf,
        entry: Arc<Mutex<RepoDebounce>>,
    ) {
        loop {
            {
                let mut state = entry.lock().unwrap();
                state.scheduled = false;
                state.running = true;
            }
            let this = self.clone();
            let path = worktree.clone();
            let result = tokio::task::spawn_blocking(move || this.reconcile_now(&path)).await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(repository = %key, %error, "watcher-triggered backlog reconciliation failed; prior snapshot retained");
                }
                Err(join_error) => {
                    tracing::warn!(repository = %key, error = %join_error, "backlog reconciliation task panicked");
                }
            }
            let rerun = {
                let mut state = entry.lock().unwrap();
                state.running = false;
                let rerun = state.rerun;
                state.rerun = false;
                rerun
            };
            if !rerun {
                break;
            }
        }
    }

    /// Bounded reconcile-on-read fallback: reconciles `worktree` at most
    /// once per `read_fallback_interval`, single-flight per repository, so
    /// polling the dashboard cannot cause a continuous full-tree scan while
    /// still repairing whatever a watcher gap missed.
    pub fn reconcile_if_stale(&self, worktree: &Path) {
        let Ok(identity) = FilesystemBacklogDiscovery.resolve(worktree) else {
            return;
        };
        let entry = {
            let mut map = self.freshness.lock().unwrap();
            map.entry(identity.key.clone())
                .or_insert_with(|| {
                    Arc::new(Mutex::new(
                        Instant::now()
                            .checked_sub(self.read_fallback_interval + Duration::from_secs(1))
                            .unwrap_or_else(Instant::now),
                    ))
                })
                .clone()
        };
        let mut last = entry.lock().unwrap();
        if last.elapsed() < self.read_fallback_interval {
            return;
        }
        // Marked attempted before the (possibly slow) reconciliation itself,
        // so a concurrent reader for the same repository sees fresh state
        // and skips rather than racing into a second reconcile.
        *last = Instant::now();
        drop(last);
        if let Err(error) = self.reconcile_now(worktree) {
            tracing::warn!(worktree = %worktree.display(), %error, "read-time backlog reconciliation failed");
        }
    }

    fn record_diagnostic(&self, repository_key: &str, message: &str) {
        self.diagnostics
            .lock()
            .unwrap()
            .insert(repository_key.to_string(), message.to_string());
        tracing::error!(repository = %repository_key, error = %message, "backlog reconciliation failed; prior snapshot retained");
    }

    fn clear_diagnostic(&self, repository_key: &str) {
        self.diagnostics.lock().unwrap().remove(repository_key);
    }

    /// The last recorded reconciliation failure for `repository_key`, if
    /// any reconciliation has failed since the last success.
    pub fn diagnostic(&self, repository_key: &str) -> Option<String> {
        self.diagnostics
            .lock()
            .unwrap()
            .get(repository_key)
            .cloned()
    }

    fn touch_freshness(&self, repository_key: &str) {
        let mut map = self.freshness.lock().unwrap();
        match map.get(repository_key) {
            Some(entry) => *entry.lock().unwrap() = Instant::now(),
            None => {
                map.insert(
                    repository_key.to_string(),
                    Arc::new(Mutex::new(Instant::now())),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::operator_ui::{OperatorAction, OperatorDataSource, OperatorQuery};
    use familiar_ai_storage::list_backlog_entries;
    use std::fs;
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
            std::process::Command::new("git")
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

    /// A temporary repository with one active PRD, ready for reconciliation.
    fn temp_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
        fs::write(
            repo.path().join("docs/prds/PRD-001.md"),
            "# PRD-1: One\n\n**Depends on:** none\n",
        )
        .unwrap();
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

    #[test]
    fn startup_reconciliation_discovers_an_existing_backlog_without_any_command() {
        let repo = temp_repo();
        let db = open_db();
        let reconciler = BacklogReconciler::new(
            db.clone(),
            config_for(repo.path()),
            Duration::from_millis(1),
            Duration::from_secs(3600),
        );
        reconciler.reconcile_all_configured();

        let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
        let entries =
            list_backlog_entries(db.lock().unwrap().conn(), &identity.key, None, None, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prd_path, "docs/prds/PRD-001.md");
        assert_eq!(entries[0].status, "pending");
    }

    #[test]
    fn is_relevant_distinguishes_prd_paths_from_ordinary_source_edits() {
        let repo = temp_repo();
        let db = open_db();
        let reconciler = BacklogReconciler::new(
            db,
            config_for(repo.path()),
            Duration::from_millis(1),
            Duration::from_secs(3600),
        );
        let prd_path = repo.path().join("docs/prds/PRD-002.md");
        assert!(reconciler.is_relevant(repo.path(), &prd_path));
        let source_path = repo.path().join("src/main.rs");
        assert!(!reconciler.is_relevant(repo.path(), &source_path));
    }

    #[test]
    fn reconcile_now_fails_closed_and_preserves_the_prior_snapshot_on_malformed_discovery() {
        let repo = temp_repo();
        let db = open_db();
        let reconciler = BacklogReconciler::new(
            db.clone(),
            config_for(repo.path()),
            Duration::from_millis(1),
            Duration::from_secs(3600),
        );
        reconciler.reconcile_now(repo.path()).unwrap();
        let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
        assert!(reconciler.diagnostic(&identity.key).is_none());

        // Corrupt the PRD so discovery fails closed (no level-one heading).
        fs::write(repo.path().join("docs/prds/PRD-001.md"), "not a prd\n").unwrap();
        let result = reconciler.reconcile_now(repo.path());
        assert!(result.is_err());
        assert!(reconciler.diagnostic(&identity.key).is_some());

        // The prior, valid snapshot is untouched.
        let entries =
            list_backlog_entries(db.lock().unwrap().conn(), &identity.key, None, None, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, "pending");
    }

    #[test]
    fn moving_a_prd_into_done_produces_a_completed_row_and_preserves_the_old_path() {
        let repo = temp_repo();
        let db = open_db();
        let reconciler = BacklogReconciler::new(
            db.clone(),
            config_for(repo.path()),
            Duration::from_millis(1),
            Duration::from_secs(3600),
        );
        reconciler.reconcile_now(repo.path()).unwrap();

        fs::create_dir_all(repo.path().join("docs/prds/done")).unwrap();
        fs::rename(
            repo.path().join("docs/prds/PRD-001.md"),
            repo.path().join("docs/prds/done/PRD-001.md"),
        )
        .unwrap();
        reconciler.reconcile_now(repo.path()).unwrap();

        let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
        let entries =
            list_backlog_entries(db.lock().unwrap().conn(), &identity.key, None, None, 10).unwrap();
        let done = entries
            .iter()
            .find(|e| e.prd_path == "docs/prds/done/PRD-001.md")
            .unwrap();
        assert_eq!(done.status, "completed");
        assert!(done.missing_since.is_none());
        let old = entries
            .iter()
            .find(|e| e.prd_path == "docs/prds/PRD-001.md")
            .unwrap();
        assert!(
            old.missing_since.is_some(),
            "the old path's history is preserved, not deleted"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_burst_of_rapid_events_collapses_into_exactly_one_reconciliation_and_one_event() {
        let repo = temp_repo();
        let db = open_db();
        let reconciler = BacklogReconciler::new(
            db.clone(),
            config_for(repo.path()),
            Duration::from_millis(80),
            Duration::from_secs(3600),
        );
        let dispatcher = Arc::new(OperatorDispatcher::new(Arc::new(NoopSource), 1));
        reconciler.set_event_sink(dispatcher.clone());

        let prd_path = repo.path().join("docs/prds/PRD-001.md");
        for _ in 0..5 {
            reconciler.observe_event(&WatcherEvent::FileChanged {
                path: prd_path.clone(),
                repo_root: Some(repo.path().to_path_buf()),
            });
        }

        tokio::time::sleep(Duration::from_millis(400)).await;
        let events = dispatcher.observe(0, 10).unwrap();
        assert_eq!(
            events.len(),
            1,
            "a burst within the debounce window must collapse into one reconciliation"
        );
        // This is exactly the shape `app.js`'s `heartbeat()` polls for
        // (`operator_observe`) and reacts to unconditionally: a contiguous
        // sequence and the daemon generation it started with. Nothing about
        // this event's transport differs from a client-issued mutation, so
        // the existing gap/restart logic refreshes on it with no changes to
        // the desktop client.
        assert_eq!(events[0].sequence, 1);
        assert_eq!(events[0].daemon_generation, 1);
        assert!(events[0].topic.starts_with("backlog_reconciled:"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unrelated_source_edits_do_not_reconcile_the_backlog() {
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
        let source_path = repo.path().join("src/main.rs");
        reconciler.observe_event(&WatcherEvent::FileChanged {
            path: source_path,
            repo_root: Some(repo.path().to_path_buf()),
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        let events = dispatcher.observe(0, 10).unwrap();
        assert!(
            events.is_empty(),
            "an ordinary source-code edit must not churn backlog state"
        );
    }

    #[test]
    fn read_fallback_repairs_changes_missed_during_watcher_downtime_and_is_bounded() {
        let repo = temp_repo();
        let db = open_db();
        let reconciler = BacklogReconciler::new(
            db.clone(),
            config_for(repo.path()),
            Duration::from_millis(1),
            Duration::from_millis(20),
        );
        let dispatcher = Arc::new(OperatorDispatcher::new(Arc::new(NoopSource), 1));
        reconciler.set_event_sink(dispatcher.clone());

        // Startup reconciliation, then a change the (unrun) watcher never saw.
        reconciler.reconcile_now(repo.path()).unwrap();
        fs::write(
            repo.path().join("docs/prds/PRD-002.md"),
            "# PRD-2: Two\n\n**Depends on:** none\n",
        )
        .unwrap();

        // Immediately after: still within the bounded interval, so this is a
        // no-op and does not yet see the new file.
        reconciler.reconcile_if_stale(repo.path());
        let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
        let entries =
            list_backlog_entries(db.lock().unwrap().conn(), &identity.key, None, None, 10).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "reconcile-on-read is bounded, not continuous"
        );

        // Once the interval has elapsed, the next read repairs the gap.
        std::thread::sleep(Duration::from_millis(30));
        reconciler.reconcile_if_stale(repo.path());
        let entries =
            list_backlog_entries(db.lock().unwrap().conn(), &identity.key, None, None, 10).unwrap();
        assert_eq!(entries.len(), 2, "the missed PRD is repaired at read time");
    }

    /// Exercises the exact path `main.rs`'s watcher handler takes: drain a
    /// real `WatcherEvent` channel and forward each event to the reconciler.
    #[tokio::test(flavor = "multi_thread")]
    async fn active_to_done_rename_observed_as_events_reconciles_to_one_completed_row() {
        let repo = temp_repo();
        let db = open_db();
        let reconciler = BacklogReconciler::new(
            db.clone(),
            config_for(repo.path()),
            Duration::from_millis(30),
            Duration::from_secs(3600),
        );
        reconciler.reconcile_now(repo.path()).unwrap();

        let (tx, mut rx) = mpsc::channel::<WatcherEvent>(16);
        let reconciler_for_task = reconciler.clone();
        let drain = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                reconciler_for_task.observe_event(&event);
            }
        });

        fs::create_dir_all(repo.path().join("docs/prds/done")).unwrap();
        let old_path = repo.path().join("docs/prds/PRD-001.md");
        let new_path = repo.path().join("docs/prds/done/PRD-001.md");
        fs::rename(&old_path, &new_path).unwrap();
        tx.send(WatcherEvent::FileRenamed {
            old_path,
            new_path,
            repo_root: Some(repo.path().to_path_buf()),
        })
        .await
        .unwrap();

        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(tx);
        let _ = drain.await;

        let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
        let entries =
            list_backlog_entries(db.lock().unwrap().conn(), &identity.key, None, None, 10).unwrap();
        let done = entries
            .iter()
            .find(|e| e.prd_path == "docs/prds/done/PRD-001.md")
            .unwrap();
        assert_eq!(done.status, "completed");
    }
}
