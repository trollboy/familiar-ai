use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Exclusive ownership of one unattended driver on this host. The file is
/// removed on orderly exit; stale PID ownership is recovered atomically.
pub struct WorkerLock {
    path: PathBuf,
}

impl WorkerLock {
    pub fn acquire(runtime_dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(runtime_dir)?;
        let path = runtime_dir.join("drive.lock");
        match create(&path) {
            Ok(()) => Ok(Self { path }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let owner = fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok());
                if owner.is_some_and(process_alive) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "another Familiar driver owns {} (pid {owner:?})",
                            path.display()
                        ),
                    ));
                }
                fs::remove_file(&path)?;
                create(&path)?;
                Ok(Self { path })
            }
            Err(error) => Err(error),
        }
    }
}

fn create(path: &Path) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    writeln!(file, "{}", std::process::id())?;
    file.sync_all()
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

impl Drop for WorkerLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_live_owner_and_recovers_stale_owner() {
        let temp = tempfile::tempdir().unwrap();
        let first = WorkerLock::acquire(temp.path()).unwrap();
        assert!(WorkerLock::acquire(temp.path()).is_err());
        drop(first);
        fs::write(temp.path().join("drive.lock"), "4294967295\n").unwrap();
        let recovered = WorkerLock::acquire(temp.path()).unwrap();
        drop(recovered);
        assert!(!temp.path().join("drive.lock").exists());
    }
}
