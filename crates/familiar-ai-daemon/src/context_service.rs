use familiar_ai_repomap::{repository_cache_key, MapError, RepositoryMap};
use familiar_ai_watcher::WatcherEvent;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct ContextService {
    maps: Arc<Mutex<BTreeMap<PathBuf, RepositoryMap>>>,
    cache_dir: Option<PathBuf>,
}

impl ContextService {
    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        Self {
            maps: Default::default(),
            cache_dir: Some(cache_dir),
        }
    }
    pub fn register(&self, repository: &Path, watch_covered: bool) {
        self.maps
            .lock()
            .unwrap()
            .entry(repository.to_owned())
            .or_insert_with(|| RepositoryMap::new(watch_covered));
    }
    pub fn map(&self, repository: &Path) -> Option<RepositoryMap> {
        self.maps.lock().unwrap().get(repository).cloned()
    }
    pub fn serialized(&self, repository: &Path, max_symbols: usize) -> Option<Vec<u8>> {
        self.map(repository).map(|m| m.serialize(max_symbols))
    }
    pub fn apply(&self, event: &WatcherEvent) {
        match event {
            WatcherEvent::RepoDiscovered { repo_root } => {
                self.register(repo_root, true);
                self.index_repository(repo_root);
            }
            WatcherEvent::FileChanged {
                path,
                repo_root: Some(repo),
            }
            | WatcherEvent::FileCreated {
                path,
                repo_root: Some(repo),
            } => {
                self.register(repo, true);
                let _ = self
                    .maps
                    .lock()
                    .unwrap()
                    .get_mut(repo)
                    .unwrap()
                    .reindex_file(repo, path);
            }
            WatcherEvent::FileRemoved {
                path,
                repo_root: Some(repo),
            } => {
                self.register(repo, true);
                let _ = self
                    .maps
                    .lock()
                    .unwrap()
                    .get_mut(repo)
                    .unwrap()
                    .remove_file(repo, path);
            }
            WatcherEvent::FileRenamed {
                old_path,
                new_path,
                repo_root: Some(repo),
            } => {
                self.register(repo, true);
                let mut guard = self.maps.lock().unwrap();
                let map = guard.get_mut(repo).unwrap();
                let _ = map.remove_file(repo, old_path);
                let _ = map.reindex_file(repo, new_path);
            }
            WatcherEvent::FileAmbiguous {
                repo_root: Some(repo),
                detail,
                ..
            } => {
                self.register(repo, true);
                self.maps
                    .lock()
                    .unwrap()
                    .get_mut(repo)
                    .unwrap()
                    .mark_stale(format!("watch event ambiguous: {detail}"));
            }
            _ => {}
        }
        if let Some(repository) = event_repository(event) {
            self.persist(repository);
        }
    }
    pub fn index_file(&self, repository: &Path, path: &Path) -> Result<(), MapError> {
        self.register(repository, true);
        self.maps
            .lock()
            .unwrap()
            .get_mut(repository)
            .unwrap()
            .reindex_file(repository, path)
    }
    fn index_repository(&self, repository: &Path) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(["ls-files", "-z"])
            .output();
        match output {
            Ok(output) if output.status.success() => {
                for file in output.stdout.split(|b| *b == 0).filter(|x| !x.is_empty()) {
                    if let Ok(file) = std::str::from_utf8(file) {
                        let _ = self.index_file(repository, &repository.join(file));
                    }
                }
            }
            _ => self
                .maps
                .lock()
                .unwrap()
                .get_mut(repository)
                .unwrap()
                .mark_stale("tracked-file inventory unavailable"),
        }
    }
    fn persist(&self, repository: &Path) {
        if let (Some(dir), Some(bytes)) = (&self.cache_dir, self.serialized(repository, 500)) {
            if std::fs::create_dir_all(dir).is_ok() {
                let path = dir.join(format!("{}.map", repository_cache_key(repository)));
                let temporary = dir.join(format!("{}.tmp", repository_cache_key(repository)));
                if std::fs::write(&temporary, bytes).is_ok() {
                    let _ = std::fs::rename(temporary, path);
                }
            }
        }
    }
}

fn event_repository(event: &WatcherEvent) -> Option<&Path> {
    match event {
        WatcherEvent::RepoDiscovered { repo_root } => Some(repo_root),
        WatcherEvent::FileChanged { repo_root, .. }
        | WatcherEvent::FileCreated { repo_root, .. }
        | WatcherEvent::FileRemoved { repo_root, .. }
        | WatcherEvent::FileRenamed { repo_root, .. }
        | WatcherEvent::FileAmbiguous { repo_root, .. } => repo_root.as_deref(),
        WatcherEvent::WatchError { .. } => None,
    }
}
