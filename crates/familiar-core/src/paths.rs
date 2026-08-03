use std::path::PathBuf;

use crate::Result;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub log_dir: PathBuf,
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
}

impl AppPaths {
    pub fn new() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::macos_paths()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::linux_paths()
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.state_dir,
            &self.runtime_dir,
            &self.log_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn linux_paths() -> Self {
        let uid = unsafe { libc::getuid() };

        let config_dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".config")
            })
            .join("familiar");

        let data_dir = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".local/share")
            })
            .join("familiar");

        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".local/state")
            })
            .join("familiar");

        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .map(|d| PathBuf::from(d).join("familiar"))
            .unwrap_or_else(|_| PathBuf::from(format!("/tmp/familiar-{uid}")));

        let log_dir = state_dir.join("log");
        let pid_path = state_dir.join("familiar.pid");
        let socket_path = runtime_dir.join("familiar.sock");

        Self {
            config_dir,
            data_dir,
            state_dir,
            runtime_dir,
            log_dir,
            socket_path,
            pid_path,
        }
    }

    #[cfg(target_os = "macos")]
    fn macos_paths() -> Self {
        let uid = unsafe { libc::getuid() };
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

        let app_support = home.join("Library/Application Support/Familiar");
        let runtime_dir = PathBuf::from(format!("/tmp/familiar-{uid}"));

        Self {
            config_dir: app_support.clone(),
            data_dir: app_support.clone(),
            state_dir: app_support.clone(),
            runtime_dir: runtime_dir.clone(),
            log_dir: home.join("Library/Logs/Familiar"),
            pid_path: app_support.join("familiar.pid"),
            socket_path: runtime_dir.join("familiar.sock"),
        }
    }
}

impl Default for AppPaths {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_not_empty() {
        let paths = AppPaths::new();
        assert!(!paths.config_dir.as_os_str().is_empty());
        assert!(!paths.data_dir.as_os_str().is_empty());
        assert!(!paths.state_dir.as_os_str().is_empty());
        assert!(!paths.runtime_dir.as_os_str().is_empty());
        assert!(!paths.log_dir.as_os_str().is_empty());
        assert!(!paths.socket_path.as_os_str().is_empty());
        assert!(!paths.pid_path.as_os_str().is_empty());
    }

    #[test]
    fn pid_path_is_under_state_dir() {
        let paths = AppPaths::new();
        assert!(paths.pid_path.starts_with(&paths.state_dir));
    }

    #[test]
    fn socket_path_is_under_runtime_dir() {
        let paths = AppPaths::new();
        assert!(paths.socket_path.starts_with(&paths.runtime_dir));
    }

    #[test]
    fn log_dir_is_under_state_dir() {
        let paths = AppPaths::new();
        assert!(paths.log_dir.starts_with(&paths.state_dir));
    }

    #[test]
    fn ensure_dirs_creates_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        let paths = AppPaths {
            config_dir: base.join("config"),
            data_dir: base.join("data"),
            state_dir: base.join("state"),
            runtime_dir: base.join("runtime"),
            log_dir: base.join("log"),
            socket_path: base.join("runtime/familiar.sock"),
            pid_path: base.join("state/familiar.pid"),
        };
        paths.ensure_dirs().unwrap();
        assert!(paths.config_dir.is_dir());
        assert!(paths.data_dir.is_dir());
        assert!(paths.state_dir.is_dir());
        assert!(paths.runtime_dir.is_dir());
        assert!(paths.log_dir.is_dir());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn respects_xdg_config_home() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("custom-config");
        std::env::set_var("XDG_CONFIG_HOME", &custom);
        let paths = AppPaths::new();
        std::env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(paths.config_dir, custom.join("familiar"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn respects_xdg_runtime_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("custom-runtime");
        std::env::set_var("XDG_RUNTIME_DIR", &custom);
        let paths = AppPaths::new();
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(paths.runtime_dir, custom.join("familiar"));
    }
}
