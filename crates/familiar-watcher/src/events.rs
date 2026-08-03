use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum WatcherEvent {
    RepoDiscovered {
        repo_root: PathBuf,
    },
    FileChanged {
        path: PathBuf,
        repo_root: Option<PathBuf>,
    },
    FileRemoved {
        path: PathBuf,
        repo_root: Option<PathBuf>,
    },
    FileRenamed {
        old_path: PathBuf,
        new_path: PathBuf,
        repo_root: Option<PathBuf>,
    },
    WatchError {
        message: String,
    },
}
