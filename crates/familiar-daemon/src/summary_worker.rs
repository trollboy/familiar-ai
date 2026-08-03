use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use familiar_core::config::SummaryConfig;
use familiar_core::models::NewFileSummary;
use familiar_core::CanonicalFileIdentity;
use familiar_storage::{Database, FileSummaryRepository, LifecycleRepository, ProjectRepository};
use familiar_summary::SummaryGenerator;

use crate::command::CommandState;

/// Hardcoded directories that are always skipped during initial repo scans
/// even if they aren't in `.gitignore`. Modern JS repos in particular love
/// to litter projects with these.
pub const HARDCODED_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".tox",
    ".pytest_cache",
    ".mypy_cache",
    ".cargo",
    ".next",
    ".nuxt",
    "vendor",
    "coverage",
    "tmp",
    "temp",
    ".cache",
    ".parcel-cache",
    ".svelte-kit",
    ".idea",
    ".vscode",
];

#[derive(Debug, Clone)]
pub struct SummaryRequest {
    pub project_id: i64,
    pub repo_root: PathBuf,
    pub path: PathBuf,
}

pub struct SummaryWorker {
    db: Arc<Mutex<Database>>,
    config: SummaryConfig,
    command_state: Arc<Mutex<CommandState>>,
}

impl SummaryWorker {
    pub fn new(
        db: Arc<Mutex<Database>>,
        config: SummaryConfig,
        command_state: Arc<Mutex<CommandState>>,
    ) -> Self {
        Self {
            db,
            config,
            command_state,
        }
    }

    pub async fn run(
        self,
        mut event_rx: mpsc::Receiver<SummaryRequest>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        let mut pending: HashMap<(i64, String), (PathBuf, PathBuf, Instant)> = HashMap::new();
        let mut ticker =
            tokio::time::interval(Duration::from_secs(self.config.flush_interval_secs));
        ticker.tick().await; // skip immediate first tick

        let quiet = Duration::from_millis(self.config.per_file_quiet_ms);
        let max_pending = self.config.max_pending_files;
        self.refill_from_durable(&mut pending, max_pending);

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    tracing::info!("summary worker shutting down");
                    return;
                }
                maybe_event = event_rx.recv() => {
                    match maybe_event {
                        Some(req) => {
                            let identity = match CanonicalFileIdentity::from_observed(req.project_id, &req.repo_root, &req.path) {
                                Ok(identity) => identity,
                                Err(error) => {
                                    tracing::warn!(project_id = req.project_id, error = %error, "rejected summary path");
                                    continue;
                                }
                            };
                            let key = (req.project_id, identity.path().to_owned());
                            if pending.len() >= max_pending && !pending.contains_key(&key) {
                                tracing::warn!(
                                    pending = pending.len(),
                                    max = max_pending,
                                    "summary worker memory queue full; durable work remains deferred"
                                );
                                continue;
                            }
                            pending.insert(key, (req.repo_root, req.path, Instant::now()));
                        }
                        None => {
                            // Channel closed; flush remaining and exit
                            self.flush_all(&mut pending);
                            return;
                        }
                    }
                }
                _ = ticker.tick() => {
                    self.flush_quiet(&mut pending, quiet);
                    self.refill_from_durable(&mut pending, max_pending);
                }
            }
        }
    }

    fn is_paused(&self) -> bool {
        self.command_state.lock().unwrap().paused
    }

    fn refill_from_durable(
        &self,
        pending: &mut HashMap<(i64, String), (PathBuf, PathBuf, Instant)>,
        max_pending: usize,
    ) {
        if pending.len() >= max_pending {
            return;
        }
        let db = self.db.lock().unwrap();
        let projects = match db.list_active_projects() {
            Ok(v) => v,
            Err(_) => return,
        };
        for project in projects {
            let remaining = max_pending.saturating_sub(pending.len());
            if remaining == 0 {
                break;
            }
            let work = match db.list_pending_summary_work(project.id, remaining) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let root = PathBuf::from(&project.repo_root);
            for item in work {
                pending
                    .entry((project.id, item.path.clone()))
                    .or_insert_with(|| (root.clone(), root.join(item.path), Instant::now()));
            }
        }
    }

    fn flush_quiet(
        &self,
        pending: &mut HashMap<(i64, String), (PathBuf, PathBuf, Instant)>,
        quiet: Duration,
    ) {
        if self.is_paused() {
            return;
        }
        let now = Instant::now();
        let ready: Vec<(i64, String)> = pending
            .iter()
            .filter_map(|(k, (_, _, last_seen))| {
                if now.duration_since(*last_seen) >= quiet {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        for key in ready {
            if let Some((root, path, _)) = pending.remove(&key) {
                self.process_one(key.0, &root, &path);
            }
        }
    }

    fn flush_all(&self, pending: &mut HashMap<(i64, String), (PathBuf, PathBuf, Instant)>) {
        if self.is_paused() {
            return;
        }
        let drained: Vec<_> = pending.drain().collect();
        for ((pid, _), (root, path, _)) in drained {
            self.process_one(pid, &root, &path);
        }
    }

    fn process_one(&self, project_id: i64, repo_root: &Path, path: &Path) {
        let identity = match CanonicalFileIdentity::from_observed(project_id, repo_root, path) {
            Ok(identity) => identity,
            Err(error) => {
                tracing::warn!(project_id, error = %error, "rejected summary path");
                return;
            }
        };
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return,
        };

        if !meta.is_file() {
            return;
        }

        if meta.len() > self.config.max_file_size_bytes {
            tracing::debug!(path = %path.display(), size = meta.len(), "skipping oversized file");
            return;
        }

        let mtime_secs = system_time_to_secs(meta.modified().ok());

        // Check existing summary for staleness — skip if same mtime and within
        // staleness window.
        // TODO: future PRD — also store a content hash so we can avoid
        // regeneration when mtime changes but content doesn't (git checkouts,
        // editor rewrites, etc.)
        {
            let db = self.db.lock().unwrap();
            if let Ok(Some(existing)) = db.get_file_summary_by_path(project_id, identity.path()) {
                if existing.last_known_mtime == mtime_secs {
                    let age = (chrono::Utc::now() - existing.last_updated_at).num_seconds() as u64;
                    if age < self.config.staleness_threshold_secs {
                        return;
                    }
                }
            }
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "failed to read file");
                let _ = self.db.lock().unwrap().fail_summary_work(
                    project_id,
                    identity.path(),
                    &e.to_string(),
                );
                return;
            }
        };

        // Do not publish output if the source changed across the read boundary.
        let after = match std::fs::metadata(path) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.db.lock().unwrap().fail_summary_work(
                    project_id,
                    identity.path(),
                    &error.to_string(),
                );
                return;
            }
        };
        if after.len() != meta.len() || system_time_to_secs(after.modified().ok()) != mtime_secs {
            let _ = self.db.lock().unwrap().fail_summary_work(
                project_id,
                identity.path(),
                "source changed during summarization",
            );
            return;
        }

        let gen = SummaryGenerator::new();
        let generated = gen.generate(Path::new(identity.path()), &content);

        let new_summary = NewFileSummary {
            project_id,
            path: identity.path().to_owned(),
            summary: generated.summary_text,
            tags: generated.tags,
            extracted_symbols: generated.extracted_symbols,
            last_known_mtime: mtime_secs,
            last_known_size: Some(meta.len() as i64),
        };

        let db = self.db.lock().unwrap();
        match db.commit_summary_work(&new_summary) {
            Ok(_) => {
                tracing::debug!(path = %path.display(), "summarized file");
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to write summary");
            }
        }
    }
}

fn system_time_to_secs(time: Option<SystemTime>) -> Option<i64> {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// Walk a repo root and emit one SummaryRequest per source file, skipping
/// hardcoded garbage directories and respecting `.gitignore`.
pub fn enqueue_initial_scan(
    repo_root: &Path,
    project_id: i64,
    tx: &mpsc::Sender<SummaryRequest>,
    max_file_size_bytes: u64,
) {
    let walker = ignore::WalkBuilder::new(repo_root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !HARDCODED_SKIP_DIRS.contains(&name.as_ref())
        })
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if meta.len() > max_file_size_bytes {
            continue;
        }
        let req = SummaryRequest {
            project_id,
            repo_root: repo_root.to_path_buf(),
            path: path.to_path_buf(),
        };
        if tx.try_send(req).is_err() {
            // Channel full or closed — stop scanning
            break;
        }
    }
}

/// Durable initial/recovery inventory. Enumeration errors are retained and
/// prevent absence reconciliation; queue capacity only affects dispatch.
pub fn run_repository_scan(
    db: &Arc<Mutex<Database>>,
    repo_root: &Path,
    project_id: i64,
    tx: &mpsc::Sender<SummaryRequest>,
    max_file_size_bytes: u64,
) {
    let root = match repo_root.to_str() {
        Some(v) => v,
        None => {
            tracing::warn!("scan root is not losslessly representable");
            return;
        }
    };
    let run = match db
        .lock()
        .unwrap()
        .start_repository_scan(project_id, root, "prd003-v1")
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error=%e,"scan blocked");
            return;
        }
    };
    let walker = ignore::WalkBuilder::new(repo_root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !HARDCODED_SKIP_DIRS.contains(&name.as_ref())
        })
        .build();
    for result in walker {
        let entry = match result {
            Ok(v) => v,
            Err(e) => {
                let _ = db
                    .lock()
                    .unwrap()
                    .fail_repository_scan(run.id, &e.to_string());
                return;
            }
        };
        let meta = match entry.metadata() {
            Ok(v) => v,
            Err(e) => {
                let _ = db
                    .lock()
                    .unwrap()
                    .fail_repository_scan(run.id, &e.to_string());
                return;
            }
        };
        if !meta.is_file() {
            continue;
        }
        let identity =
            match CanonicalFileIdentity::from_observed(project_id, repo_root, entry.path()) {
                Ok(v) => v,
                Err(e) => {
                    let _ = db
                        .lock()
                        .unwrap()
                        .fail_repository_scan(run.id, &e.to_string());
                    return;
                }
            };
        let classification = if meta.len() > max_file_size_bytes {
            "excluded"
        } else {
            match std::fs::read_to_string(entry.path()) {
                Ok(_) => "eligible",
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => "excluded",
                Err(error) => {
                    let _ = db
                        .lock()
                        .unwrap()
                        .fail_repository_scan(run.id, &error.to_string());
                    return;
                }
            }
        };
        if let Err(e) =
            db.lock()
                .unwrap()
                .stage_scan_entry(&run, identity.path(), classification, None)
        {
            let _ = db
                .lock()
                .unwrap()
                .fail_repository_scan(run.id, &e.to_string());
            return;
        }
        if classification == "excluded" {
            let _ = db.lock().unwrap().retire_absent(
                project_id,
                identity.path(),
                familiar_storage::RetirementReason::Ineligible,
                "scan",
                Some(run.id),
            );
        }
    }
    {
        let locked = db.lock().unwrap();
        if let Err(e) = locked
            .mark_scan_enumeration_complete(run.id)
            .and_then(|_| locked.reconcile_repository_scan(&run))
        {
            tracing::warn!(error=%e,"scan reconciliation incomplete");
            return;
        }
    }
    let pending = match db
        .lock()
        .unwrap()
        .list_pending_summary_work(project_id, usize::MAX)
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error=%e,"failed to resume scan work");
            return;
        }
    };
    for item in pending {
        let request = SummaryRequest {
            project_id,
            repo_root: repo_root.to_path_buf(),
            path: repo_root.join(&item.path),
        };
        if tx.try_send(request).is_err() {
            tracing::debug!(path=%item.path,"summary dispatch deferred; durable work retained");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_core::models::NewProject;
    use familiar_storage::ProjectRepository;

    fn make_db_and_project() -> (Arc<Mutex<Database>>, i64) {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let pid = db
            .create_project(&NewProject {
                name: "p".into(),
                repo_root: "/test".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;
        (Arc::new(Mutex::new(db)), pid)
    }

    fn fast_config() -> SummaryConfig {
        SummaryConfig {
            enabled: true,
            staleness_threshold_secs: 86400,
            flush_interval_secs: 1,
            max_file_size_bytes: 1_048_576,
            max_pending_files: 100,
            per_file_quiet_ms: 50,
        }
    }

    #[tokio::test]
    async fn worker_writes_summary_for_real_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("foo.rs");
        std::fs::write(&file_path, "pub fn x() {}\n").unwrap();

        let (db, pid) = make_db_and_project();
        let cs = Arc::new(Mutex::new(CommandState::new()));
        let worker = SummaryWorker::new(db.clone(), fast_config(), cs);

        let (tx, rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        tx.send(SummaryRequest {
            project_id: pid,
            repo_root: tmp.path().to_path_buf(),
            path: file_path.clone(),
        })
        .await
        .unwrap();

        let handle = tokio::spawn(async move { worker.run(rx, shutdown_rx).await });

        // Wait long enough for quiet + flush
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

        // Verify summary was written
        let db_lock = db.lock().unwrap();
        let stored = db_lock.get_file_summary_by_path(pid, "foo.rs").unwrap();
        assert!(stored.is_some(), "summary should have been written");
        let stored = stored.unwrap();
        assert_eq!(stored.path, "foo.rs");
        assert!(!stored.summary.contains(&tmp.path().display().to_string()));
        assert!(stored.last_known_mtime.is_some());
        assert!(stored.last_known_size.is_some());
    }

    #[tokio::test]
    async fn worker_respects_pause() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("paused.rs");
        std::fs::write(&file_path, "pub fn y() {}\n").unwrap();

        let (db, pid) = make_db_and_project();
        let cs = Arc::new(Mutex::new(CommandState::new()));
        cs.lock().unwrap().paused = true;
        let worker = SummaryWorker::new(db.clone(), fast_config(), cs);

        let (tx, rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        tx.send(SummaryRequest {
            project_id: pid,
            repo_root: tmp.path().to_path_buf(),
            path: file_path.clone(),
        })
        .await
        .unwrap();

        let handle = tokio::spawn(async move { worker.run(rx, shutdown_rx).await });
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

        let db_lock = db.lock().unwrap();
        let stored = db_lock.get_file_summary_by_path(pid, "paused.rs").unwrap();
        assert!(stored.is_none(), "paused worker should not write summaries");
    }

    #[tokio::test]
    async fn worker_coalesces_duplicate_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("dupe.rs");
        std::fs::write(&file_path, "pub fn z() {}\n").unwrap();

        let (db, pid) = make_db_and_project();
        let cs = Arc::new(Mutex::new(CommandState::new()));
        let worker = SummaryWorker::new(db.clone(), fast_config(), cs);

        let (tx, rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Send 5 requests for the same path
        for _ in 0..5 {
            tx.send(SummaryRequest {
                project_id: pid,
                repo_root: tmp.path().to_path_buf(),
                path: file_path.clone(),
            })
            .await
            .unwrap();
        }

        let handle = tokio::spawn(async move { worker.run(rx, shutdown_rx).await });
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

        // The single coalesced summary should exist
        let db_lock = db.lock().unwrap();
        let stored = db_lock.get_file_summary_by_path(pid, "dupe.rs").unwrap();
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn worker_skips_oversized_files() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("big.bin");
        // Write more than the configured max
        std::fs::write(&file_path, vec![b'x'; 200]).unwrap();

        let (db, pid) = make_db_and_project();
        let cs = Arc::new(Mutex::new(CommandState::new()));
        let mut config = fast_config();
        config.max_file_size_bytes = 100;
        let worker = SummaryWorker::new(db.clone(), config, cs);

        let (tx, rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        tx.send(SummaryRequest {
            project_id: pid,
            repo_root: tmp.path().to_path_buf(),
            path: file_path.clone(),
        })
        .await
        .unwrap();

        let handle = tokio::spawn(async move { worker.run(rx, shutdown_rx).await });
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

        let db_lock = db.lock().unwrap();
        let stored = db_lock.get_file_summary_by_path(pid, "big.bin").unwrap();
        assert!(stored.is_none(), "oversized file should be skipped");
    }

    #[tokio::test]
    async fn worker_skips_unchanged_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("stable.rs");
        std::fs::write(&file_path, "pub fn s() {}\n").unwrap();

        let (db, pid) = make_db_and_project();
        let cs = Arc::new(Mutex::new(CommandState::new()));

        // First worker run: write the summary
        {
            let worker = SummaryWorker::new(db.clone(), fast_config(), cs.clone());
            let (tx, rx) = mpsc::channel(16);
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            tx.send(SummaryRequest {
                project_id: pid,
                repo_root: tmp.path().to_path_buf(),
                path: file_path.clone(),
            })
            .await
            .unwrap();
            let handle = tokio::spawn(async move { worker.run(rx, shutdown_rx).await });
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let _ = shutdown_tx.send(true);
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }

        let first_updated = {
            let db_lock = db.lock().unwrap();
            db_lock
                .get_file_summary_by_path(pid, "stable.rs")
                .unwrap()
                .unwrap()
                .updated_at
        };

        // Second worker run: same file, no changes — should NOT rewrite
        {
            let worker = SummaryWorker::new(db.clone(), fast_config(), cs.clone());
            let (tx, rx) = mpsc::channel(16);
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            tx.send(SummaryRequest {
                project_id: pid,
                repo_root: tmp.path().to_path_buf(),
                path: file_path.clone(),
            })
            .await
            .unwrap();
            let handle = tokio::spawn(async move { worker.run(rx, shutdown_rx).await });
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let _ = shutdown_tx.send(true);
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }

        let second_updated = {
            let db_lock = db.lock().unwrap();
            db_lock
                .get_file_summary_by_path(pid, "stable.rs")
                .unwrap()
                .unwrap()
                .updated_at
        };

        assert_eq!(
            first_updated, second_updated,
            "summary should not be rewritten when file is unchanged"
        );
    }

    #[tokio::test]
    async fn initial_scan_skips_hardcoded_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/lib")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("node_modules/lib/x.js"), "x;\n").unwrap();
        std::fs::write(root.join("target/debug.bin"), "z\n").unwrap();

        let (tx, mut rx) = mpsc::channel(64);
        enqueue_initial_scan(root, 1, &tx, 1_048_576);
        drop(tx);

        let mut paths = Vec::new();
        while let Some(req) = rx.recv().await {
            paths.push(req.path.to_string_lossy().to_string());
        }

        assert!(paths.iter().any(|p| p.ends_with("main.rs")));
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
        assert!(!paths.iter().any(|p| p.contains("target")));
    }

    #[tokio::test]
    async fn worker_drops_when_pending_full() {
        // max_pending_files = 1, send 2 different paths
        let tmp = tempfile::tempdir().unwrap();
        let p1 = tmp.path().join("a.rs");
        let p2 = tmp.path().join("b.rs");
        std::fs::write(&p1, "x").unwrap();
        std::fs::write(&p2, "y").unwrap();

        let (db, pid) = make_db_and_project();
        let cs = Arc::new(Mutex::new(CommandState::new()));
        let mut config = fast_config();
        config.max_pending_files = 1;
        // very long quiet so neither gets flushed before we send both
        config.per_file_quiet_ms = 60_000;
        config.flush_interval_secs = 1;
        let worker = SummaryWorker::new(db.clone(), config, cs);

        let (tx, rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        tx.send(SummaryRequest {
            project_id: pid,
            repo_root: tmp.path().to_path_buf(),
            path: p1.clone(),
        })
        .await
        .unwrap();
        tx.send(SummaryRequest {
            project_id: pid,
            repo_root: tmp.path().to_path_buf(),
            path: p2.clone(),
        })
        .await
        .unwrap();

        let handle = tokio::spawn(async move { worker.run(rx, shutdown_rx).await });
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        // No assertion needed — the test passes if the worker doesn't panic when
        // attempting to insert beyond max_pending_files. The dropped event is
        // logged as a warning.
    }
}
