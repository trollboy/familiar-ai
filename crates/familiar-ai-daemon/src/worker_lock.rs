use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use familiar_ai_core::control_plane::{OwnershipClaim, CONTROL_PROTOCOL_VERSION};
use ring::rand::{SecureRandom, SystemRandom};

/// The one mutation claim shared by daemon hosting and CLI fallback. The
/// repository argument remains for source compatibility but ownership is
/// intentionally per installation, never per repository.
pub struct WorkerLock {
    path: PathBuf,
    claim: OwnershipClaim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimState {
    Absent,
    Live(OwnershipClaim),
    Stale(OwnershipClaim),
    Invalid(String),
}

impl WorkerLock {
    pub fn inspect(runtime_dir: &Path) -> io::Result<ClaimState> {
        let path = runtime_dir.join("control-plane.claim");
        let text = match fs::read_to_string(path) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ClaimState::Absent),
            Err(error) => return Err(error),
        };
        match serde_json::from_str::<OwnershipClaim>(&text) {
            Ok(claim) if claim_process_matches(&claim) => Ok(ClaimState::Live(claim)),
            Ok(claim) => Ok(ClaimState::Stale(claim)),
            Err(error) => Ok(ClaimState::Invalid(error.to_string())),
        }
    }
    pub fn acquire(runtime_dir: &Path) -> io::Result<Self> {
        Self::acquire_with_socket(runtime_dir, &runtime_dir.join("control-plane.sock"))
    }

    pub fn acquire_with_socket(runtime_dir: &Path, socket_path: &Path) -> io::Result<Self> {
        Self::acquire_inner(runtime_dir, socket_path)
    }

    pub fn acquire_repository(runtime_dir: &Path, _repository_key: &str) -> io::Result<Self> {
        Self::acquire(runtime_dir)
    }

    fn acquire_inner(runtime_dir: &Path, socket_path: &Path) -> io::Result<Self> {
        fs::create_dir_all(runtime_dir)?;
        let path = runtime_dir.join("control-plane.claim");
        let persisted_generation = fs::read_to_string(runtime_dir.join("control-plane.generation"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let generation = match fs::read_to_string(&path) {
            Ok(original) => {
                if let Ok(existing) = serde_json::from_str::<OwnershipClaim>(&original) {
                    if claim_process_matches(&existing) {
                        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!(
                            "Familiar control-plane owner pid {} is live; socket state must be diagnosed and explicit recovery used", existing.owner_pid)));
                    }
                    recover_exact(
                        &path,
                        &original,
                        existing
                            .generation
                            .max(persisted_generation)
                            .saturating_add(1),
                    )?
                } else if original
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .is_some_and(process_alive)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "legacy Familiar owner is live; stop or upgrade it before recovery",
                    ));
                } else {
                    recover_exact(&path, &original, persisted_generation.saturating_add(1))?
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => persisted_generation.saturating_add(1),
            Err(e) => return Err(e),
        };
        let claim = new_claim(runtime_dir, socket_path, generation)?;
        match create(&path, &claim) {
            Ok(()) => {
                let generation_path = runtime_dir.join("control-plane.generation");
                fs::write(&generation_path, format!("{generation}\n"))?;
                OpenOptions::new()
                    .read(true)
                    .open(&generation_path)?
                    .sync_all()?;
                Ok(Self { path, claim })
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Familiar is already running; another process won the control-plane ownership race",
            )),
            Err(e) => Err(e),
        }
    }

    pub fn claim(&self) -> &OwnershipClaim {
        &self.claim
    }
}

fn recover_exact(path: &Path, original: &str, generation: u64) -> io::Result<u64> {
    let guard = path.with_extension("recovery");
    let guard_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&guard)
        .map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "another process is recovering the stale control-plane claim",
                )
            } else {
                e
            }
        })?;
    let result = (|| {
        if !matches!(fs::read_to_string(path), Ok(current) if current == original) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "control-plane claim changed during recovery",
            ));
        }
        fs::remove_file(path)?;
        Ok(generation)
    })();
    drop(guard_file);
    let _ = fs::remove_file(guard);
    result
}

fn new_claim(
    runtime_dir: &Path,
    socket_path: &Path,
    generation: u64,
) -> io::Result<OwnershipClaim> {
    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| io::Error::other("secure owner nonce generation failed"))?;
    let nonce = random.iter().map(|b| format!("{b:02x}")).collect();
    let installation_path = runtime_dir.join("installation-id");
    let installation_id = match fs::read_to_string(&installation_path) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_owned(),
        _ => {
            let mut bytes = [0_u8; 16];
            SystemRandom::new()
                .fill(&mut bytes)
                .map_err(|_| io::Error::other("secure installation identity generation failed"))?;
            let value = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&installation_path)
            {
                Ok(mut file) => {
                    writeln!(file, "{value}")?;
                    file.sync_all()?;
                    value
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    fs::read_to_string(&installation_path)?.trim().to_owned()
                }
                Err(e) => return Err(e),
            }
        }
    };
    Ok(OwnershipClaim {
        installation_id,
        owner_nonce: nonce,
        owner_pid: std::process::id(),
        process_start_identity: process_start_identity(std::process::id())
            .unwrap_or_else(|| "unavailable".into()),
        boot_identity: boot_identity(),
        socket_path: socket_path.to_string_lossy().into_owned(),
        protocol_version: CONTROL_PROTOCOL_VERSION,
        generation,
    })
}

fn create(path: &Path, claim: &OwnershipClaim) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer(&mut file, claim).map_err(io::Error::other)?;
    writeln!(file)?;
    file.sync_all()
}

fn claim_process_matches(claim: &OwnershipClaim) -> bool {
    process_alive(claim.owner_pid)
        && process_start_identity(claim.owner_pid).as_deref() == Some(&claim.process_start_identity)
}

#[cfg(target_os = "linux")]
fn process_start_identity(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()?
        .split_whitespace()
        .nth(21)
        .map(str::to_owned)
}
#[cfg(not(target_os = "linux"))]
fn process_start_identity(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}
#[cfg(target_os = "linux")]
fn boot_identity() -> Option<String> {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|s| s.trim().to_owned())
}
#[cfg(not(target_os = "linux"))]
fn boot_identity() -> Option<String> {
    std::process::Command::new("sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .or_else(|| std::env::var("SECURITYSESSIONID").ok())
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let r = unsafe { libc::kill(pid as i32, 0) };
    r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
#[cfg(not(unix))]
fn process_alive(_: u32) -> bool {
    true
}

impl Drop for WorkerLock {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str::<OwnershipClaim>(&s).ok())
            .as_ref()
            == Some(&self.claim)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_live_owner_and_recovers_stale_owner() {
        let t = tempfile::tempdir().unwrap();
        let first = WorkerLock::acquire(t.path()).unwrap();
        assert!(WorkerLock::acquire(t.path()).is_err());
        let p = first.path.clone();
        drop(first);
        fs::write(&p, "4294967295\n").unwrap();
        let recovered = WorkerLock::acquire(t.path()).unwrap();
        assert_eq!(recovered.claim.generation, 2);
    }
    #[test]
    fn claim_contains_non_pid_identity() {
        let t = tempfile::tempdir().unwrap();
        let lock = WorkerLock::acquire(t.path()).unwrap();
        assert_eq!(lock.claim.owner_nonce.len(), 64);
        assert!(!lock.claim.process_start_identity.is_empty());
    }

    #[test]
    fn inspection_distinguishes_absent_live_and_stale() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(WorkerLock::inspect(t.path()).unwrap(), ClaimState::Absent);
        let lock = WorkerLock::acquire(t.path()).unwrap();
        assert!(matches!(
            WorkerLock::inspect(t.path()).unwrap(),
            ClaimState::Live(_)
        ));
        let mut stale = lock.claim().clone();
        stale.process_start_identity = "definitely-not-this-process".into();
        std::fs::write(&lock.path, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert!(matches!(
            WorkerLock::inspect(t.path()).unwrap(),
            ClaimState::Stale(_)
        ));
    }

    #[test]
    fn simultaneous_fallback_claims_have_exactly_one_winner() {
        let temp = tempfile::tempdir().unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let path = temp.path().to_path_buf();
            let start = start.clone();
            threads.push(std::thread::spawn(move || {
                start.wait();
                WorkerLock::acquire(&path).ok()
            }));
        }
        let claims = threads
            .into_iter()
            .filter_map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(claims.len(), 1);
        assert!(matches!(
            WorkerLock::inspect(temp.path()).unwrap(),
            ClaimState::Live(_)
        ));
    }
}
