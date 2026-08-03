use std::path::Path;
use std::sync::Arc;

use notify::event::{ModifyKind, RenameMode};
use notify::{Config, Event, EventKind, PollWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use familiar_core::config::WatcherConfig;
use familiar_core::FamiliarError;

use crate::discovery::{self, RepoMap};
use crate::events::WatcherEvent;

struct IgnoreMatcher {
    matcher: Option<ignore::gitignore::Gitignore>,
}

impl IgnoreMatcher {
    fn is_ignored(&self, path: &Path) -> bool {
        match &self.matcher {
            Some(gi) => gi.matched(path, path.is_dir()).is_ignore(),
            None => false,
        }
    }
}

fn build_ignore_matcher(patterns: &[String]) -> IgnoreMatcher {
    if patterns.is_empty() {
        return IgnoreMatcher { matcher: None };
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new("");
    for pattern in patterns {
        builder.add_line(None, pattern).ok();
    }
    IgnoreMatcher {
        matcher: builder.build().ok(),
    }
}

pub struct FileWatcher {
    config: WatcherConfig,
}

impl FileWatcher {
    pub fn new(config: WatcherConfig) -> Self {
        Self { config }
    }

    pub async fn run(
        self,
        tx: mpsc::Sender<WatcherEvent>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> familiar_core::Result<()> {
        // 1. Initial discovery
        let repo_map = discovery::scan_for_repos(&self.config);
        tracing::info!(count = repo_map.len(), "initial repository scan complete");

        for root in repo_map.roots() {
            let _ = tx
                .send(WatcherEvent::RepoDiscovered {
                    repo_root: root.clone(),
                })
                .await;
        }

        // Resolve actual watch paths
        let watch_paths = discovery::resolve_watch_paths(&self.config);
        if watch_paths.is_empty() {
            tracing::info!("no valid watch paths configured, watcher idle");
            let _ = shutdown.changed().await;
            return Ok(());
        }

        // 2. Build ignore matcher from custom patterns
        let ignore_matcher = build_ignore_matcher(&self.config.ignore_patterns);

        // 3. Use notify directly: the mini debouncer discards the native kind.
        let (bridge_tx, bridge_rx) = std::sync::mpsc::channel();
        let mut watcher = PollWatcher::new(
            move |result| {
                let _ = bridge_tx.send(result);
            },
            Config::default().with_poll_interval(std::time::Duration::from_millis(
                self.config.debounce_ms.max(25),
            )),
        )
        .map_err(|e| FamiliarError::Watcher(e.to_string()))?;

        // 4. Add watches on resolved paths
        for path in &watch_paths {
            watcher.watch(path, RecursiveMode::Recursive).map_err(|e| {
                FamiliarError::Watcher(format!("failed to watch {}: {e}", path.display()))
            })?;
            tracing::info!(path = %path.display(), "watching directory");
        }

        // 5. Spawn bridge thread: reads from std::sync::mpsc, sends to tokio mpsc
        let repo_map = Arc::new(repo_map);
        let tx_clone = tx.clone();
        let bridge_handle = std::thread::spawn({
            let repo_map = repo_map.clone();
            move || {
                bridge_thread(bridge_rx, tx_clone, &repo_map, &ignore_matcher);
            }
        });

        // 6. Wait for shutdown
        let _ = shutdown.changed().await;
        tracing::info!("watcher shutting down");

        // Drop debouncer to stop its internal thread, which closes bridge_tx,
        // which causes bridge_rx.recv() to return Err, ending the bridge thread.
        drop(watcher);
        let _ = bridge_handle.join();

        Ok(())
    }
}

fn bridge_thread(
    rx: std::sync::mpsc::Receiver<Result<Event, notify::Error>>,
    tx: mpsc::Sender<WatcherEvent>,
    repo_map: &RepoMap,
    ignore_matcher: &IgnoreMatcher,
) {
    while let Ok(result) = rx.recv() {
        match result {
            Ok(event) => {
                let paths: Vec<_> = event
                    .paths
                    .into_iter()
                    .filter(|p| !ignore_matcher.is_ignored(p))
                    .collect();
                if paths.is_empty() {
                    continue;
                }
                let repo_root = paths
                    .first()
                    .and_then(|p| repo_map.find_repo(p))
                    .map(Path::to_path_buf);
                let translated = translate_native(event.kind, paths, repo_root);
                if tx.blocking_send(translated).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.blocking_send(WatcherEvent::WatchError {
                    message: e.to_string(),
                });
            }
        }
    }
}

fn translate_native(
    kind: EventKind,
    paths: Vec<std::path::PathBuf>,
    repo_root: Option<std::path::PathBuf>,
) -> WatcherEvent {
    match kind {
        EventKind::Create(_) if paths.len() == 1 => WatcherEvent::FileCreated {
            path: paths[0].clone(),
            repo_root,
        },
        EventKind::Remove(_) if paths.len() == 1 => WatcherEvent::FileRemoved {
            path: paths[0].clone(),
            repo_root,
        },
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if paths.len() == 2 => {
            WatcherEvent::FileRenamed {
                old_path: paths[0].clone(),
                new_path: paths[1].clone(),
                repo_root,
            }
        }
        EventKind::Modify(ModifyKind::Name(_)) => WatcherEvent::FileAmbiguous {
            paths,
            repo_root,
            detail: "unpaired or platform-ambiguous rename".into(),
        },
        EventKind::Modify(_) if paths.len() == 1 => WatcherEvent::FileChanged {
            path: paths[0].clone(),
            repo_root,
        },
        kind => WatcherEvent::FileAmbiguous {
            paths,
            repo_root,
            detail: format!("unsupported or coalesced native event: {kind:?}"),
        },
    }
}

#[cfg(test)]
mod native_translation_tests {
    use super::*;

    #[test]
    fn exact_pair_is_rename_and_unpaired_is_ambiguous() {
        let old = std::path::PathBuf::from("/repo/old.rs");
        let new = std::path::PathBuf::from("/repo/new.rs");
        assert!(
            matches!(translate_native(EventKind::Modify(ModifyKind::Name(RenameMode::Both)),vec![old.clone(),new.clone()],Some("/repo".into())),WatcherEvent::FileRenamed{old_path,new_path,..} if old_path==old && new_path==new)
        );
        assert!(matches!(
            translate_native(
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                vec![old],
                Some("/repo".into())
            ),
            WatcherEvent::FileAmbiguous { .. }
        ));
    }

    #[test]
    fn create_modify_and_remove_remain_typed() {
        let path = std::path::PathBuf::from("/repo/a.rs");
        assert!(matches!(
            translate_native(
                EventKind::Create(notify::event::CreateKind::File),
                vec![path.clone()],
                None
            ),
            WatcherEvent::FileCreated { .. }
        ));
        assert!(matches!(
            translate_native(
                EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
                vec![path.clone()],
                None
            ),
            WatcherEvent::FileChanged { .. }
        ));
        assert!(matches!(
            translate_native(
                EventKind::Remove(notify::event::RemoveKind::File),
                vec![path],
                None
            ),
            WatcherEvent::FileRemoved { .. }
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    #[tokio::test]
    async fn watcher_emits_file_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        // Create a repo so events have context
        fs::create_dir_all(root.join("repo/.git")).unwrap();

        let config = WatcherConfig {
            enabled: true,
            paths: vec![root.clone()],
            debounce_ms: 100,
            ignore_patterns: vec![],
            respect_gitignore: false,
        };

        let (tx, mut rx) = mpsc::channel(64);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let watcher = FileWatcher::new(config);
        let handle = tokio::spawn(async move { watcher.run(tx, shutdown_rx).await });

        // Wait for watcher to initialize
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Drain RepoDiscovered events
        while let Ok(event) = rx.try_recv() {
            if matches!(event, WatcherEvent::RepoDiscovered { .. }) {
                continue;
            }
        }

        // Create a file to trigger an event
        fs::write(root.join("repo/test.txt"), "hello").unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut found = false;
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, WatcherEvent::FileCreated { ref path, ref repo_root } | WatcherEvent::FileChanged { ref path, ref repo_root } if path.ends_with("test.txt") && repo_root.is_some())
            {
                found = true;
                break;
            }
        }
        assert!(found, "watcher did not emit a typed event for test.txt");

        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    async fn watcher_respects_ignore_patterns() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();

        let config = WatcherConfig {
            enabled: true,
            paths: vec![root.clone()],
            debounce_ms: 100,
            ignore_patterns: vec!["**/target/**".into()],
            respect_gitignore: false,
        };

        let (tx, mut rx) = mpsc::channel(64);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let watcher = FileWatcher::new(config);
        let handle = tokio::spawn(async move { watcher.run(tx, shutdown_rx).await });

        tokio::time::sleep(Duration::from_millis(500)).await;

        // Drain any initial events
        while rx.try_recv().is_ok() {}

        // Write to ignored path only
        fs::write(root.join("target/debug/output.o"), "binary").unwrap();

        // Collect all events for 1.5 seconds
        let mut events = vec![];
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            events.push(event);
        }

        // No FileChanged events should reference target paths
        let target_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, WatcherEvent::FileChanged { path, .. } if path.to_string_lossy().contains("target")))
            .collect();
        assert!(
            target_events.is_empty(),
            "should not receive events for files in target/: {target_events:?}"
        );

        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    async fn watcher_no_paths_idles() {
        let config = WatcherConfig {
            enabled: true,
            paths: vec![],
            debounce_ms: 100,
            ..Default::default()
        };

        let (tx, _rx) = mpsc::channel(64);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let watcher = FileWatcher::new(config);
        let handle = tokio::spawn(async move { watcher.run(tx, shutdown_rx).await });

        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = shutdown_tx.send(true);

        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("watcher should exit")
            .expect("watcher task panicked");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn watcher_shutdown_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        let config = WatcherConfig {
            enabled: true,
            paths: vec![root],
            debounce_ms: 100,
            ignore_patterns: vec![],
            respect_gitignore: false,
        };

        let (tx, _rx) = mpsc::channel(64);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let watcher = FileWatcher::new(config);
        let handle = tokio::spawn(async move { watcher.run(tx, shutdown_rx).await });

        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = shutdown_tx.send(true);

        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("watcher should exit within timeout")
            .expect("watcher task panicked");
        assert!(result.is_ok());
    }

    #[test]
    fn ignore_matcher_filters_patterns() {
        let matcher = build_ignore_matcher(&["target/**".into(), "*.log".into()]);
        assert!(matcher.is_ignored(Path::new("target/debug/build")));
        assert!(matcher.is_ignored(Path::new("app.log")));
        assert!(!matcher.is_ignored(Path::new("src/main.rs")));
    }

    #[test]
    fn ignore_matcher_empty_patterns() {
        let matcher = build_ignore_matcher(&[]);
        assert!(!matcher.is_ignored(Path::new("anything")));
    }
}
