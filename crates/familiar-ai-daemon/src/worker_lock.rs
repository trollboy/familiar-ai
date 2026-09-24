use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use familiar_ai_core::control_plane::{OwnershipClaim, CONTROL_PROTOCOL_VERSION};
use familiar_ai_storage::{LeaseAcquireOutcome, LeaseRenewOutcome};
use ring::rand::{SecureRandom, SystemRandom};

use crate::control_plane::ControlPlaneService;

/// The one mutation claim shared by daemon hosting and CLI fallback. The
/// repository argument remains for source compatibility but ownership is
/// intentionally per installation, never per repository.
/// The environment variable by which the control-plane owner tells a command
/// it spawned that it acts on the owner's behalf.
///
/// A command the owner dispatched is the owner's own work. Without this it
/// contends with the parent that launched it and can never succeed: the tray's
/// Start and Re-drive both dispatch `familiar-ai run`/`resume` through the
/// control plane, and both acquire this lock, and the daemon dispatching them
/// already holds it (FAM-BUG-062).
pub const DELEGATION_ENV: &str = "FAMILIAR_AI_DELEGATED_BY";

/// Whether this process was spawned by the live owner named in the claim.
///
/// Deliberately narrow: the variable must name the *same* pid the claim
/// records, and that pid must still be the live owner. An inherited or stale
/// value therefore authorises nothing.
fn delegated_by(claim: &OwnershipClaim) -> bool {
    std::env::var(DELEGATION_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .is_some_and(|pid| pid == claim.owner_pid)
}

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

/// Refuse a direct state mutation while a driver owns the control plane.
///
/// Operator tools write checkpoint rows and worktrees straight through the
/// storage API, bypassing the claim the drive respects. Doing that under a
/// live session risks mutating candidates the driver is actively working
/// (FAM-BUG-048) — this is the courtesy the drive already extends itself.
pub fn refuse_while_driver_owns(runtime_dir: &Path, action: &str) -> Result<(), String> {
    match WorkerLock::inspect(runtime_dir) {
        Ok(ClaimState::Live(claim)) => Err(format!(
            "refusing to {action}: Familiar control-plane owner pid {} is live. \
             Wait for it to finish, or stop it first.",
            claim.owner_pid
        )),
        Ok(_) => Ok(()),
        // An unreadable claim is not proof of absence; say so rather than
        // proceeding on a guess.
        Err(error) => Err(format!(
            "refusing to {action}: cannot read the control-plane claim ({error})"
        )),
    }
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
                    // FAM-BUG-093: a command the owner dispatched never
                    // recovers the owner's claim. If the pid that delegated
                    // us is alive, we are its delegate whether or not the
                    // start-identity probe agrees (on macOS that probe shells
                    // out to `ps`, which a sandboxed, env-cleared child may
                    // not reproduce byte for byte). If it is dead, the work
                    // it delegated is orphaned and stops here; it does not
                    // become the new owner and then delete the claim on exit,
                    // which is how a wave launch read as "ownership is stale"
                    // and then "daemon is not running".
                    if delegated_by(&existing) {
                        if process_alive(existing.owner_pid) {
                            return Self::acquire_delegation(runtime_dir, &existing);
                        }
                        return Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            format!(
                                "control-plane owner pid {} that delegated this command is no longer running",
                                existing.owner_pid
                            ),
                        ));
                    }
                    if claim_process_matches(&existing) {
                        // A command the owner dispatched runs as the owner's
                        // delegate rather than contending with it. Exclusion
                        // is preserved on both sides: anyone who is not the
                        // owner's child is still refused below, and two
                        // delegates still exclude each other through their own
                        // O_EXCL lock.
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

    /// Take the delegation lock on behalf of a live owner.
    ///
    /// Separate from the claim so the owner keeps its own, and `O_EXCL` so two
    /// delegates cannot run at once — which is the property the claim exists
    /// to guarantee and the one a naive bypass would have thrown away.
    fn acquire_delegation(runtime_dir: &Path, owner: &OwnershipClaim) -> io::Result<Self> {
        let path = runtime_dir.join("control-plane.delegate");
        if let Ok(existing) = fs::read_to_string(&path) {
            if let Ok(claim) = serde_json::from_str::<OwnershipClaim>(&existing) {
                if claim_process_matches(&claim) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "another delegate of control-plane owner pid {} is live (pid {})",
                            owner.owner_pid, claim.owner_pid
                        ),
                    ));
                }
            }
            // The previous delegate is gone; its lock is not authority.
            let _ = fs::remove_file(&path);
        }
        let claim = OwnershipClaim {
            owner_pid: std::process::id(),
            process_start_identity: process_start_identity(std::process::id()).unwrap_or_default(),
            ..owner.clone()
        };
        let body = serde_json::to_string(&claim)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        io::Write::write_all(&mut file, body.as_bytes())?;
        file.sync_all()?;
        Ok(Self { path, claim })
    }

    pub fn claim(&self) -> &OwnershipClaim {
        &self.claim
    }
}

/// A durable, cross-host, per-repository driver lease (PRD-091). Distinct
/// from `WorkerLock`: the filesystem claim above stays exactly as it was and
/// keeps doing local, per-installation mutual exclusion; `HostLease` is the
/// additional database-backed authority that a second machine can also see,
/// so two hosts driving the same repository against the same store still
/// refuse each other. Two hosts driving *different* repositories never
/// contend, since each lease is keyed by repository.
pub struct HostLease {
    repository_key: String,
    holder_token: String,
    control: ControlPlaneService,
    live: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    renewal_thread: Option<std::thread::JoinHandle<()>>,
}

impl HostLease {
    /// A stable identifier for this machine. Best-effort: an unresolvable
    /// hostname still yields mutual exclusion (a second host would need to
    /// coincidentally share the fallback string), it just loses the
    /// human-readable holder name in a refusal message.
    pub fn host_identity() -> String {
        host_identity()
    }

    /// Acquire the lease for `repository_key`, or fail naming the current
    /// holder and its remaining life if another host or process holds it
    /// live. On success, a background thread renews on `renewal_interval`
    /// until the lease is released or lost.
    pub fn acquire(
        control: &ControlPlaneService,
        repository_key: &str,
        renewal_interval: Duration,
        ttl_secs: i64,
    ) -> Result<Self, String> {
        Self::acquire_as(
            control,
            repository_key,
            &host_identity(),
            renewal_interval,
            ttl_secs,
        )
    }

    /// As `acquire`, with an explicit host identity rather than the local
    /// machine's hostname. Production code should call `acquire`; this exists
    /// so tests can simulate distinct hosts sharing one store from a single
    /// process.
    pub fn acquire_as(
        control: &ControlPlaneService,
        repository_key: &str,
        host_identity: &str,
        renewal_interval: Duration,
        ttl_secs: i64,
    ) -> Result<Self, String> {
        let host_identity = host_identity.to_owned();
        let process_identity = local_process_identity();
        let holder_token = random_token().map_err(|e| e.to_string())?;
        match control
            .acquire_repository_lease(
                repository_key,
                &host_identity,
                &process_identity,
                &holder_token,
                ttl_secs,
            )
            .map_err(|e| e.to_string())?
        {
            LeaseAcquireOutcome::Acquired(_) => {}
            LeaseAcquireOutcome::Refused {
                holder_host,
                holder_process,
                remaining_ms,
            } => {
                return Err(format!(
                    "refusing to drive '{repository_key}': the lease is held by host {holder_host} process {holder_process} for {remaining_ms}ms more"
                ));
            }
        }
        let live = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let renewal_thread = {
            let control = control.clone();
            let repository_key = repository_key.to_owned();
            let host_identity = host_identity.clone();
            let process_identity = process_identity.clone();
            let holder_token = holder_token.clone();
            let live = live.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                renewal_loop(
                    control,
                    repository_key,
                    host_identity,
                    process_identity,
                    holder_token,
                    ttl_secs,
                    renewal_interval,
                    live,
                    stop,
                )
            })
        };
        Ok(Self {
            repository_key: repository_key.to_owned(),
            holder_token,
            control: control.clone(),
            live,
            stop,
            renewal_thread: Some(renewal_thread),
        })
    }

    /// Whether this host still holds the lease, as of its last renewal
    /// attempt. Once false it never becomes true again — a fresh `acquire`
    /// is required.
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
}

#[allow(clippy::too_many_arguments)]
fn renewal_loop(
    control: ControlPlaneService,
    repository_key: String,
    host_identity: String,
    process_identity: String,
    holder_token: String,
    ttl_secs: i64,
    renewal_interval: Duration,
    live: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    const POLL: Duration = Duration::from_millis(50);
    loop {
        let mut waited = Duration::ZERO;
        while waited < renewal_interval {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let step = POLL.min(renewal_interval - waited);
            std::thread::sleep(step);
            waited += step;
        }
        if stop.load(Ordering::Acquire) {
            return;
        }
        let renewed = control.renew_repository_lease(
            &repository_key,
            &host_identity,
            &process_identity,
            &holder_token,
            ttl_secs,
        );
        match renewed {
            Ok(LeaseRenewOutcome::Renewed(_)) => {}
            // Lost (expiry, an out-of-band takeover) or an unreachable store:
            // both fail the driver closed within one renewal interval rather
            // than let a partitioned host keep writing durable state.
            Ok(LeaseRenewOutcome::Lost { .. }) | Err(_) => {
                live.store(false, Ordering::Release);
                return;
            }
        }
    }
}

impl Drop for HostLease {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.renewal_thread.take() {
            let _ = thread.join();
        }
        if self.live.load(Ordering::Acquire) {
            let _ = self
                .control
                .release_repository_lease(&self.repository_key, &self.holder_token);
        }
    }
}

fn random_token() -> io::Result<String> {
    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| io::Error::other("secure lease token generation failed"))?;
    Ok(random.iter().map(|b| format!("{b:02x}")).collect())
}

fn local_process_identity() -> String {
    let pid = std::process::id();
    format!(
        "{pid}:{}",
        process_start_identity(pid).unwrap_or_else(|| "unavailable".into())
    )
}

#[cfg(unix)]
fn host_identity() -> String {
    let mut buffer = [0_u8; 256];
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result != 0 {
        return "unknown-host".into();
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}
#[cfg(not(unix))]
fn host_identity() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown-host".into())
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
    // FAM-BUG-027: the claim must appear ATOMICALLY. Writing JSON into an
    // O_EXCL-created file leaves a window where a concurrent claimant reads
    // an empty/partial file, judges it corrupt, "recovers" it — deleting the
    // live winner's claim — and claims too: two owners of an exclusive lock.
    // Write-and-sync a unique temporary, then hard-link into place: link
    // fails with AlreadyExists exactly like O_EXCL, and no reader can ever
    // observe a partial claim.
    static CREATE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temporary = path.with_extension(format!(
        "claim-tmp-{}-{}",
        std::process::id(),
        CREATE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, claim).map_err(io::Error::other)?;
        writeln!(file)?;
        file.sync_all()?;
        fs::hard_link(&temporary, path)
    })();
    let _ = fs::remove_file(&temporary);
    result
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
    /// FAM-BUG-093: the owner's own delegate must never recover the claim,
    /// even when the start-identity probe disagrees with what the claim
    /// recorded. Serialised with the other env-touching tests by the lock.
    #[test]
    fn a_delegate_never_recovers_a_live_owners_claim() {
        let _guard = env_lock().lock().unwrap();
        let t = tempfile::tempdir().unwrap();
        let owner = WorkerLock::acquire(t.path()).unwrap();
        let claim_path = t.path().join("control-plane.claim");
        // Corrupt the recorded identity: the probe can no longer match it.
        let mut recorded: OwnershipClaim =
            serde_json::from_str(&fs::read_to_string(&claim_path).unwrap()).unwrap();
        recorded.process_start_identity = "something-else".into();
        let corrupted = serde_json::to_string(&recorded).unwrap();
        fs::write(&claim_path, &corrupted).unwrap();

        std::env::set_var(DELEGATION_ENV, std::process::id().to_string());
        let delegate = WorkerLock::acquire(t.path());
        std::env::remove_var(DELEGATION_ENV);

        let delegate = delegate.expect("a delegate of a live owner acquires");
        assert!(t.path().join("control-plane.delegate").exists());
        assert_eq!(
            fs::read_to_string(&claim_path).unwrap(),
            corrupted,
            "the owner's claim is untouched"
        );
        drop(delegate);
        assert_eq!(
            fs::read_to_string(&claim_path).unwrap(),
            corrupted,
            "dropping the delegate removes only its own file"
        );
        assert!(!t.path().join("control-plane.delegate").exists());
        drop(owner);
    }

    /// A delegate whose owner has died stops; it does not become the owner.
    #[test]
    fn a_delegate_of_a_dead_owner_stops_instead_of_taking_over() {
        let _guard = env_lock().lock().unwrap();
        let t = tempfile::tempdir().unwrap();
        let claim_path = t.path().join("control-plane.claim");
        let owner = WorkerLock::acquire(t.path()).unwrap();
        let mut recorded: OwnershipClaim =
            serde_json::from_str(&fs::read_to_string(&claim_path).unwrap()).unwrap();
        drop(owner);
        recorded.owner_pid = 4_294_967_294;
        let dead = serde_json::to_string(&recorded).unwrap();
        fs::write(&claim_path, &dead).unwrap();

        std::env::set_var(DELEGATION_ENV, recorded.owner_pid.to_string());
        let result = WorkerLock::acquire(t.path());
        std::env::remove_var(DELEGATION_ENV);

        let error = match result {
            Ok(_) => panic!("an orphaned delegate must not acquire"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("no longer running"), "{error}");
        assert_eq!(fs::read_to_string(&claim_path).unwrap(), dead);
    }

    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
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
        // FAM-BUG-027 regression: repeated so the pre-fix torn-read window
        // (a concurrent claimant reading a half-written claim, "recovering"
        // it, and becoming a second owner) is statistically visible. With
        // the atomic hard-link claim it must be exactly one winner, always.
        for _ in 0..25 {
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
            assert_eq!(claims.len(), 1, "exactly one owner per race");
            assert!(matches!(
                WorkerLock::inspect(temp.path()).unwrap(),
                ClaimState::Live(_)
            ));
        }
    }
}
