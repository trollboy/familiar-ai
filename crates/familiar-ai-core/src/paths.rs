use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{FamiliarError, Result};

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

/// Outcome of the deterministic one-time identity migration for one
/// persistent directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// The new-identifier path already exists; the old path is ignored
    /// entirely (already migrated or fresh install).
    UsedNew,
    /// The old directory was atomically renamed to the new path.
    Migrated,
    /// Neither path exists; initialization proceeds lazily as before.
    Fresh,
}

/// One-time `familiar` -> `familiar-ai` migration for a single persistent
/// directory. Fail-closed: any rename error stops startup; nothing is ever
/// copied, merged, or partially moved.
pub fn migrate_directory(
    old: &Path,
    new: &Path,
    audit: &mut dyn Write,
) -> Result<MigrationOutcome> {
    match (old.exists(), new.exists()) {
        (true, true) => {
            return Err(FamiliarError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "both legacy and new state directories exist: {} and {}; \
                     resolve manually — Familiar will not guess which is authoritative",
                    old.display(),
                    new.display()
                ),
            )))
        }
        (false, true) => return Ok(MigrationOutcome::UsedNew),
        (false, false) => return Ok(MigrationOutcome::Fresh),
        (true, false) => {}
    }
    std::fs::rename(old, new).map_err(|source| {
        FamiliarError::Io(std::io::Error::new(
            source.kind(),
            format!(
                "identity migration failed renaming {} -> {}: {source}",
                old.display(),
                new.display()
            ),
        ))
    })?;
    writeln!(
        audit,
        "identity migration: {} -> {}",
        old.display(),
        new.display()
    )
    .map_err(FamiliarError::Io)?;
    Ok(MigrationOutcome::Migrated)
}

impl AppPaths {
    /// Pure path computation under the `familiar-ai` identity. Startup code
    /// must use [`AppPaths::resolve`], which also performs the one-time
    /// legacy-state migration.
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

    /// Startup path resolution: migrate legacy `familiar` state to the
    /// `familiar-ai` locations (fail-closed), then return the new paths.
    /// Runtime and tmp-fallback directories are ephemeral and are recreated,
    /// never migrated. Explicit user-configured absolute overrides are read
    /// from configuration afterwards and are never touched here.
    pub fn resolve() -> Result<Self> {
        Self::resolve_with_audit(&mut std::io::stderr())
    }

    pub fn resolve_with_audit(audit: &mut dyn Write) -> Result<Self> {
        let new = Self::new();
        let legacy = Self::legacy_paths();
        // Persistent directories only; deduplicated because macOS maps
        // config, data, and state onto one Application Support directory.
        // Parents are visited before nested children (log under state).
        let mut seen = BTreeSet::new();
        for (old, new) in [
            (&legacy.config_dir, &new.config_dir),
            (&legacy.data_dir, &new.data_dir),
            (&legacy.state_dir, &new.state_dir),
            (&legacy.log_dir, &new.log_dir),
        ] {
            if seen.insert((old.clone(), new.clone())) {
                migrate_directory(old, new, audit)?;
            }
        }
        Ok(new)
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

    /// The pre-rename identifier layout, retained solely so [`AppPaths::resolve`]
    /// can migrate existing state.
    /// identity-gate exception: legacy identifiers are intentional here.
    fn legacy_paths() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::macos_layout("Familiar", "familiar") // identity-gate: allow
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::linux_layout("familiar") // identity-gate: allow
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn linux_paths() -> Self {
        Self::linux_layout("familiar-ai")
    }

    #[cfg(not(target_os = "macos"))]
    fn linux_layout(identity: &str) -> Self {
        let uid = unsafe { libc::getuid() };

        let config_dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".config")
            })
            .join(identity);

        let data_dir = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".local/share")
            })
            .join(identity);

        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".local/state")
            })
            .join(identity);

        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .map(|d| PathBuf::from(d).join(identity))
            .unwrap_or_else(|_| PathBuf::from(format!("/tmp/{identity}-{uid}")));

        let log_dir = state_dir.join("log");
        let pid_path = state_dir.join(format!("{identity}.pid"));
        let socket_path = runtime_dir.join(format!("{identity}.sock"));

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
        Self::macos_layout("Familiar-AI", "familiar-ai")
    }

    #[cfg(target_os = "macos")]
    fn macos_layout(app_identity: &str, file_identity: &str) -> Self {
        let uid = unsafe { libc::getuid() };
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

        let app_support = home.join("Library/Application Support").join(app_identity);
        let runtime_dir = PathBuf::from(format!("/tmp/{file_identity}-{uid}"));

        Self {
            config_dir: app_support.clone(),
            data_dir: app_support.clone(),
            state_dir: app_support.clone(),
            runtime_dir: runtime_dir.clone(),
            log_dir: home.join("Library/Logs").join(app_identity),
            pid_path: app_support.join(format!("{file_identity}.pid")),
            socket_path: runtime_dir.join(format!("{file_identity}.sock")),
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
    fn identifiers_use_the_new_identity() {
        let paths = AppPaths::new();
        assert!(paths
            .pid_path
            .file_name()
            .is_some_and(|name| name == "familiar-ai.pid"));
        assert!(paths
            .socket_path
            .file_name()
            .is_some_and(|name| name == "familiar-ai.sock"));
        #[cfg(not(target_os = "macos"))]
        {
            assert!(paths.config_dir.ends_with("familiar-ai"));
            assert!(paths.data_dir.ends_with("familiar-ai"));
            assert!(paths.state_dir.ends_with("familiar-ai"));
        }
        #[cfg(target_os = "macos")]
        {
            assert!(paths.config_dir.ends_with("Familiar-AI"));
            assert!(paths.log_dir.ends_with("Familiar-AI"));
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn tmp_fallback_uses_new_identity() {
        let previous = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::remove_var("XDG_RUNTIME_DIR");
        let paths = AppPaths::new();
        if let Some(value) = previous {
            std::env::set_var("XDG_RUNTIME_DIR", value);
        }
        let uid = unsafe { libc::getuid() };
        assert_eq!(
            paths.runtime_dir,
            PathBuf::from(format!("/tmp/familiar-ai-{uid}"))
        );
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
        #[cfg(not(target_os = "macos"))]
        assert!(paths.log_dir.starts_with(&paths.state_dir));
        #[cfg(target_os = "macos")]
        assert!(!paths.log_dir.as_os_str().is_empty());
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
            socket_path: base.join("runtime/familiar-ai.sock"),
            pid_path: base.join("state/familiar-ai.pid"),
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
        assert_eq!(paths.config_dir, custom.join("familiar-ai"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn respects_xdg_runtime_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("custom-runtime");
        std::env::set_var("XDG_RUNTIME_DIR", &custom);
        let paths = AppPaths::new();
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(paths.runtime_dir, custom.join("familiar-ai"));
    }

    #[test]
    fn migration_uses_existing_new_path_when_old_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old");
        let new = tmp.path().join("new");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("current.txt"), "current").unwrap();
        let mut audit = Vec::new();
        assert_eq!(
            migrate_directory(&old, &new, &mut audit).unwrap(),
            MigrationOutcome::UsedNew
        );
        assert!(audit.is_empty());
        assert!(new.join("current.txt").exists());
    }

    #[test]
    fn migration_fails_closed_when_both_directories_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old");
        let new = tmp.path().join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("legacy.txt"), "legacy").unwrap();
        std::fs::write(new.join("current.txt"), "current").unwrap();
        let mut audit = Vec::new();
        let error = migrate_directory(&old, &new, &mut audit).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(&old.display().to_string()));
        assert!(message.contains(&new.display().to_string()));
        assert!(audit.is_empty());
        // Nothing moved, merged, deleted, or opened for write.
        assert!(old.join("legacy.txt").exists());
        assert!(new.join("current.txt").exists());
    }

    #[test]
    fn migration_renames_atomically_with_exactly_one_audit_line() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old");
        let new = tmp.path().join("new");
        std::fs::create_dir_all(&old).unwrap();
        let database = old.join("familiar.db");
        std::fs::write(&database, b"sqlite bytes \x00\x01\x02").unwrap();
        let before = std::fs::read(&database).unwrap();
        let mut audit = Vec::new();
        assert_eq!(
            migrate_directory(&old, &new, &mut audit).unwrap(),
            MigrationOutcome::Migrated
        );
        assert!(!old.exists());
        let after = std::fs::read(new.join("familiar.db")).unwrap();
        assert_eq!(before, after, "database file must be byte-identical");
        let audit = String::from_utf8(audit).unwrap();
        assert_eq!(audit.lines().count(), 1);
        assert_eq!(
            audit.trim(),
            format!("identity migration: {} -> {}", old.display(), new.display())
        );
    }

    #[test]
    fn migration_is_fresh_when_neither_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let mut audit = Vec::new();
        assert_eq!(
            migrate_directory(&tmp.path().join("old"), &tmp.path().join("new"), &mut audit)
                .unwrap(),
            MigrationOutcome::Fresh
        );
        assert!(audit.is_empty());
    }

    #[test]
    fn migration_rename_errors_propagate_without_partial_state() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("data.txt"), "data").unwrap();
        // Destination parent does not exist: rename must fail verbatim.
        let new = tmp.path().join("missing-parent/new");
        let mut audit = Vec::new();
        let error = migrate_directory(&old, &new, &mut audit).unwrap_err();
        assert!(error.to_string().contains("identity migration failed"));
        assert!(audit.is_empty());
        assert!(old.join("data.txt").exists(), "no partial migration");
    }
}
