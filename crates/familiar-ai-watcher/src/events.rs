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
    FileCreated {
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
    FileAmbiguous {
        paths: Vec<PathBuf>,
        repo_root: Option<PathBuf>,
        detail: String,
    },
    WatchError {
        message: String,
    },
}
