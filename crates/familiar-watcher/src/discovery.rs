use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use familiar_core::config::WatcherConfig;

/// Ordered set of repo roots for fast path-prefix lookup.
pub struct RepoMap {
    roots: BTreeSet<PathBuf>,
}

impl RepoMap {
    pub fn new() -> Self {
        Self {
            roots: BTreeSet::new(),
        }
    }

    pub fn insert(&mut self, root: PathBuf) {
        self.roots.insert(root);
    }

    /// Given a file path, find the repo root it belongs to by walking ancestors.
    pub fn find_repo(&self, path: &Path) -> Option<&Path> {
        let mut current = path;
        loop {
            if let Some(root) = self.roots.get(current) {
                return Some(root.as_path());
            }
            match current.parent() {
                Some(parent) if parent != current => current = parent,
                _ => return None,
            }
        }
    }

    pub fn roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.roots.iter()
    }

    pub fn len(&self) -> usize {
        self.roots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

impl Default for RepoMap {
    fn default() -> Self {
        Self::new()
    }
}

/// Scan configured watch paths for Git repositories.
/// Returns a RepoMap containing all discovered repo roots.
pub fn scan_for_repos(config: &WatcherConfig) -> RepoMap {
    let mut map = RepoMap::new();

    for watch_path in &config.paths {
        let resolved = resolve_tilde(watch_path);

        if !resolved.is_dir() {
            tracing::warn!(path = %resolved.display(), "watch path is not a directory, skipping");
            continue;
        }

        let walker = WalkBuilder::new(&resolved)
            .hidden(false)
            .git_ignore(config.respect_gitignore)
            .git_global(config.respect_gitignore)
            .git_exclude(config.respect_gitignore)
            .max_depth(Some(5))
            .build();

        for entry in walker.flatten() {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                let path = entry.path();
                if path.file_name().is_some_and(|n| n == ".git") {
                    if let Some(repo_root) = path.parent() {
                        let repo_root = repo_root.to_path_buf();
                        tracing::info!(repo = %repo_root.display(), "discovered git repository");
                        map.insert(repo_root);
                    }
                }
            }
        }
    }

    map
}

fn resolve_tilde(path: &Path) -> PathBuf {
    if path.starts_with("~") {
        if let Some(home) = dirs::home_dir() {
            if let Ok(rest) = path.strip_prefix("~") {
                return home.join(rest);
            }
        }
    }
    path.to_path_buf()
}

/// Resolve watch paths, expanding ~ to home directory.
pub fn resolve_watch_paths(config: &WatcherConfig) -> Vec<PathBuf> {
    config
        .paths
        .iter()
        .map(|p| resolve_tilde(p))
        .filter(|p: &PathBuf| {
            if p.is_dir() {
                true
            } else {
                tracing::warn!(path = %p.display(), "watch path is not a directory, skipping");
                false
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_map_find_repo() {
        let mut map = RepoMap::new();
        map.insert(PathBuf::from("/home/user/projects/foo"));
        map.insert(PathBuf::from("/home/user/projects/bar"));

        assert_eq!(
            map.find_repo(Path::new("/home/user/projects/foo/src/main.rs")),
            Some(Path::new("/home/user/projects/foo"))
        );
        assert_eq!(
            map.find_repo(Path::new("/home/user/projects/bar/Cargo.toml")),
            Some(Path::new("/home/user/projects/bar"))
        );
    }

    #[test]
    fn repo_map_no_match() {
        let mut map = RepoMap::new();
        map.insert(PathBuf::from("/home/user/projects/foo"));

        assert!(map
            .find_repo(Path::new("/home/user/other/file.txt"))
            .is_none());
        assert!(map.find_repo(Path::new("/tmp/random")).is_none());
    }

    #[test]
    fn repo_map_exact_root_match() {
        let mut map = RepoMap::new();
        map.insert(PathBuf::from("/home/user/projects/foo"));

        assert_eq!(
            map.find_repo(Path::new("/home/user/projects/foo")),
            Some(Path::new("/home/user/projects/foo"))
        );
    }

    #[test]
    fn scan_finds_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Create repo structures
        std::fs::create_dir_all(root.join("project-a/.git")).unwrap();
        std::fs::create_dir_all(root.join("project-b/.git")).unwrap();
        std::fs::create_dir_all(root.join("not-a-repo/src")).unwrap();

        let config = WatcherConfig {
            paths: vec![root.to_path_buf()],
            respect_gitignore: false,
            ..Default::default()
        };

        let map = scan_for_repos(&config);
        assert_eq!(map.len(), 2);
        assert!(map.find_repo(&root.join("project-a/src/main.rs")).is_some());
        assert!(map.find_repo(&root.join("project-b/Cargo.toml")).is_some());
    }

    #[test]
    fn scan_nonexistent_path() {
        let config = WatcherConfig {
            paths: vec![PathBuf::from("/nonexistent/path/that/does/not/exist")],
            ..Default::default()
        };

        let map = scan_for_repos(&config);
        assert!(map.is_empty());
    }

    #[test]
    fn scan_nested_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("parent/.git")).unwrap();
        std::fs::create_dir_all(root.join("parent/child/.git")).unwrap();

        let config = WatcherConfig {
            paths: vec![root.to_path_buf()],
            respect_gitignore: false,
            ..Default::default()
        };

        let map = scan_for_repos(&config);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn repo_map_len_and_empty() {
        let map = RepoMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        let mut map = RepoMap::new();
        map.insert(PathBuf::from("/test"));
        assert!(!map.is_empty());
        assert_eq!(map.len(), 1);
    }
}
