use std::fs;
use std::path::Path;

use familiar_core::{FamiliarError, Result};

pub fn write_pid_file(path: &Path) -> Result<()> {
    // Check for existing PID file
    if path.exists() {
        if let Ok(contents) = fs::read_to_string(path) {
            if let Ok(pid) = contents.trim().parse::<i32>() {
                // Check if process is still alive
                let alive = unsafe { libc::kill(pid, 0) } == 0;
                if alive {
                    return Err(FamiliarError::AlreadyRunning);
                }
            }
        }
        // Stale PID file — remove it
        fs::remove_file(path).ok();
    }

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, format!("{}", std::process::id()))?;
    Ok(())
}

pub fn remove_pid_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_and_remove_pid_file() {
        let tmp = tempdir().unwrap();
        let pid_path = tmp.path().join("test.pid");

        write_pid_file(&pid_path).unwrap();
        assert!(pid_path.exists());

        let contents = fs::read_to_string(&pid_path).unwrap();
        assert_eq!(contents, format!("{}", std::process::id()));

        remove_pid_file(&pid_path).unwrap();
        assert!(!pid_path.exists());
    }

    #[test]
    fn remove_nonexistent_pid_file_is_ok() {
        let tmp = tempdir().unwrap();
        let pid_path = tmp.path().join("nonexistent.pid");
        remove_pid_file(&pid_path).unwrap();
    }

    #[test]
    fn detects_already_running() {
        let tmp = tempdir().unwrap();
        let pid_path = tmp.path().join("test.pid");

        // Write our own PID — we are alive
        fs::write(&pid_path, format!("{}", std::process::id())).unwrap();

        let result = write_pid_file(&pid_path);
        assert!(matches!(result, Err(FamiliarError::AlreadyRunning)));
    }

    #[test]
    fn stale_pid_file_is_overwritten() {
        let tmp = tempdir().unwrap();
        let pid_path = tmp.path().join("test.pid");

        // Write a PID that almost certainly doesn't exist
        fs::write(&pid_path, "999999999").unwrap();

        write_pid_file(&pid_path).unwrap();
        let contents = fs::read_to_string(&pid_path).unwrap();
        assert_eq!(contents, format!("{}", std::process::id()));
    }

    #[test]
    fn creates_parent_directory() {
        let tmp = tempdir().unwrap();
        let pid_path = tmp.path().join("nested/dir/test.pid");

        write_pid_file(&pid_path).unwrap();
        assert!(pid_path.exists());
    }
}
