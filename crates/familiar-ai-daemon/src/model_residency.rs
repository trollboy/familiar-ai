//! PRD-073 warm local model residency: the PRD-056 daemon holds configured
//! PRD-062 model artifacts loaded between executions so PRD-063's local
//! calls reach loaded weights and a warm prefix cache instead of paying the
//! load-and-warm tax per call.
//!
//! Three rules shape everything here:
//!
//! 1. **Budgeted.** Residency is bounded by an explicit resident count and,
//!    optionally, a declared memory ceiling. Admitting a new resident at a
//!    ceiling evicts the least recently used one and records the eviction.
//! 2. **Audited.** Every transition — loaded, evicted, health-failed,
//!    restarted, failed, stopped — is an append-only
//!    `model_residency_events` row. The fourth acceptance criterion is
//!    specifically that a dead resident never silently degrades into
//!    per-call loading, so [`ResidencyOutcome::NotResident`] is always
//!    accompanied by either a recorded event or a stated configuration
//!    reason.
//! 3. **Off by default.** With residency disabled the manager starts
//!    nothing and [`ResidencyManager::directory`] stays empty, which makes
//!    routing byte-identical to PRD-063's configured-endpoint behavior.
//!
//! The manager is deliberately synchronous: launching, killing and probing
//! serving processes is blocking work. The daemon runs [`run`], which calls
//! it from `spawn_blocking` on the configured health interval.

use std::collections::{BTreeMap, HashMap};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use familiar_ai_core::config::{
    Config, LocalRuntimeKind, ModelResidencyConfig, ResidencyCeiling, ResidentModelConfig,
    WorkerRegistryConfig,
};
use familiar_ai_llm::residency::{ResidencyDirectory, ResidencyState, ResidentEndpoint};
use familiar_ai_storage::repos::model_artifact::ModelArtifactRepository;
use familiar_ai_storage::repos::model_residency::{
    ModelResidencyRepository, ResidencyEvent, ResidencyEventRow,
};
use familiar_ai_storage::Database;

/// Everything needed to start and address one resident server, resolved
/// from the residency entry plus the PRD-063 worker it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentLaunchSpec {
    pub resident_key: String,
    pub worker_identity: String,
    pub model_artifact_id: String,
    pub runtime_id: String,
    pub base_url: String,
    pub launch: Vec<String>,
    pub memory_mb: Option<u64>,
    pub ready_timeout_secs: u64,
}

/// Starting, stopping and probing a serving process.
///
/// This is a trait because the two things residency must prove — that a
/// second call incurs no second load, and that a dead resident is detected,
/// restarted a bounded number of times, and then failed loudly — are
/// properties of the *manager*, not of any particular serving runtime.
/// Tests supply a fake; the daemon supplies [`ProcessResidentLauncher`].
pub trait ResidentServerLauncher: Send + Sync {
    /// Starts the serving process for `spec`, addressed thereafter by
    /// `server_identity`.
    fn launch(&self, spec: &ResidentLaunchSpec, server_identity: &str) -> Result<(), String>;
    /// Stops it. Best-effort: an already-dead process is not an error.
    fn stop(&self, server_identity: &str) -> Result<(), String>;
    /// Liveness. Must fail for a process that has exited *and* for one that
    /// is alive but no longer answering, since both forfeit residency.
    fn probe(&self, server_identity: &str, base_url: &str) -> Result<(), String>;
}

/// Where one execution should be routed and what that implies for the
/// record it will write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResidencyOutcome {
    /// An already-loaded resident took the call: no load was paid.
    Warm {
        server_identity: String,
        base_url: String,
    },
    /// The model was loaded for this call and is now held for the next one.
    Cold {
        server_identity: String,
        base_url: String,
    },
    /// No resident serves this call. `reason` always names why — a disabled
    /// switch, an unregistered artifact, an exhausted resident — so the
    /// caller's fall back to per-call loading is never unexplained.
    NotResident { reason: String },
}

impl ResidencyOutcome {
    pub fn residency_state(&self) -> ResidencyState {
        match self {
            Self::Warm { .. } => ResidencyState::Warm,
            Self::Cold { .. } | Self::NotResident { .. } => ResidencyState::Cold,
        }
    }

    pub fn server_identity(&self) -> Option<&str> {
        match self {
            Self::Warm {
                server_identity, ..
            }
            | Self::Cold {
                server_identity, ..
            } => Some(server_identity),
            Self::NotResident { .. } => None,
        }
    }
}

struct Resident {
    spec: ResidentLaunchSpec,
    server_identity: String,
    /// Monotonic use counter rather than a timestamp: LRU order must be
    /// exact, and two loads inside the same clock tick must still order.
    last_used: u64,
}

/// The residency reasons this module writes, named in one place so the
/// durable record and the operator-facing message never drift apart.
pub mod reasons {
    pub const DISABLED: &str = "residency-disabled";
    pub const ARTIFACT_NOT_REGISTERED: &str = "artifact-not-registered";
    pub const NOT_CONFIGURED: &str = "resident-not-configured";
    pub const WORKER_MISSING: &str = "worker-not-configured";
    pub const RESTARTS_EXHAUSTED: &str = "health-restart-exhausted";
    pub const HEALTH_PROBE_FAILED: &str = "health-probe-failed";
    pub const LAUNCH_FAILED: &str = "launch-failed";
    pub const READY_TIMEOUT: &str = "ready-timeout";
    pub const DISABLED_BY_CONFIGURATION: &str = "disabled-by-configuration";
    pub const DAEMON_SHUTDOWN: &str = "daemon-shutdown";
    pub const EXCEEDS_MEMORY_CEILING: &str = "resident-exceeds-memory-ceiling";
}

pub struct ResidencyManager {
    config: ModelResidencyConfig,
    launcher: Arc<dyn ResidentServerLauncher>,
    residents: BTreeMap<String, Resident>,
    /// Residents that exhausted their restart budget, with the named reason
    /// they failed. A failed key is never silently relaunched.
    failed: BTreeMap<String, String>,
    /// Consecutive launch/health failures per resident key, reset by a
    /// successful load.
    failure_attempts: BTreeMap<String, u32>,
    uses: u64,
    identity_counter: u64,
}

impl ResidencyManager {
    pub fn new(config: ModelResidencyConfig, launcher: Arc<dyn ResidentServerLauncher>) -> Self {
        Self {
            config,
            launcher,
            residents: BTreeMap::new(),
            failed: BTreeMap::new(),
            failure_attempts: BTreeMap::new(),
            uses: 0,
            identity_counter: 0,
        }
    }

    pub fn config(&self) -> &ModelResidencyConfig {
        &self.config
    }

    /// Replaces the effective residency configuration. Callers follow this
    /// with [`Self::reconcile`] so residents the new configuration no longer
    /// enables are actually stopped and recorded.
    pub fn set_config(&mut self, config: ModelResidencyConfig) {
        self.config = config;
    }

    /// The routing view: which workers currently have a resident server.
    /// Empty whenever residency is off, which is what makes the disabled
    /// path identical to PRD-063's.
    pub fn directory(&self) -> ResidencyDirectory {
        let mut directory = ResidencyDirectory::new();
        for resident in self.residents.values() {
            directory.insert(
                &resident.spec.worker_identity,
                ResidentEndpoint {
                    server_identity: resident.server_identity.clone(),
                    base_url: resident.spec.base_url.clone(),
                },
            );
        }
        directory
    }

    pub fn resident_keys(&self) -> Vec<String> {
        self.residents.keys().cloned().collect()
    }

    pub fn failure_reason(&self, resident_key: &str) -> Option<&str> {
        self.failed.get(resident_key).map(String::as_str)
    }

    /// Clears a terminal failure so the next call may try again. Explicit by
    /// design: an exhausted resident stays refused until something decides
    /// otherwise, rather than thrashing a broken serving runtime.
    pub fn clear_failure(&mut self, resident_key: &str) -> bool {
        self.failure_attempts.remove(resident_key);
        self.failed.remove(resident_key).is_some()
    }

    fn next_server_identity(&mut self, resident_key: &str) -> String {
        self.identity_counter += 1;
        format!("{resident_key}#{}", self.identity_counter)
    }

    fn record(
        &self,
        db: &Database,
        spec: &ResidentLaunchSpec,
        server_identity: &str,
        event: ResidencyEvent,
        reason: Option<&str>,
        restart_attempt: Option<u32>,
    ) -> familiar_ai_core::Result<String> {
        ModelResidencyRepository::new(db.conn()).record(&ResidencyEventRow {
            resident_key: &spec.resident_key,
            worker_identity: &spec.worker_identity,
            model_artifact_id: &spec.model_artifact_id,
            runtime_id: &spec.runtime_id,
            server_identity,
            event,
            reason,
            restart_attempt,
            memory_mb: spec.memory_mb,
        })
    }

    /// Resolves a residency entry against the worker registry it names.
    /// Validation already refused a resident whose worker is absent or
    /// carries no local profile or artifact; this repeats the checks because
    /// the manager is handed an effective configuration at runtime and must
    /// refuse rather than assume.
    pub fn launch_spec(
        resident_key: &str,
        resident: &ResidentModelConfig,
        registry: Option<&WorkerRegistryConfig>,
    ) -> Result<ResidentLaunchSpec, String> {
        let worker = registry
            .and_then(|registry| registry.workers.get(&resident.worker))
            .ok_or_else(|| format!("{}:{}", reasons::WORKER_MISSING, resident.worker))?;
        let local = worker
            .local
            .as_ref()
            .ok_or_else(|| format!("{}:{}", reasons::WORKER_MISSING, resident.worker))?;
        let artifact = worker
            .model_artifact
            .as_ref()
            .filter(|artifact| !artifact.trim().is_empty())
            .ok_or_else(|| format!("{}:{}", reasons::ARTIFACT_NOT_REGISTERED, resident.worker))?;
        Ok(ResidentLaunchSpec {
            resident_key: resident_key.to_string(),
            worker_identity: resident.worker.clone(),
            model_artifact_id: artifact.clone(),
            runtime_id: match local.runtime_kind {
                LocalRuntimeKind::Ollama => "ollama".to_string(),
                LocalRuntimeKind::Unsloth => "unsloth".to_string(),
            },
            base_url: local.endpoint.base_url.clone(),
            launch: resident.launch.clone(),
            memory_mb: resident.memory_mb,
            ready_timeout_secs: resident.ready_timeout_secs,
        })
    }

    /// Routes one execution, loading and holding the model if it is not
    /// already resident.
    pub fn ensure_resident(
        &mut self,
        db: &Database,
        registry: Option<&WorkerRegistryConfig>,
        resident_key: &str,
    ) -> familiar_ai_core::Result<ResidencyOutcome> {
        if !self.config.enabled {
            return Ok(ResidencyOutcome::NotResident {
                reason: reasons::DISABLED.into(),
            });
        }
        let Some(entry) = self.config.residents.get(resident_key).cloned() else {
            return Ok(ResidencyOutcome::NotResident {
                reason: format!("{}:{resident_key}", reasons::NOT_CONFIGURED),
            });
        };
        if !entry.enabled {
            return Ok(ResidencyOutcome::NotResident {
                reason: reasons::DISABLED.into(),
            });
        }
        if let Some(reason) = self.failed.get(resident_key) {
            // The failure is already a durable `failed` row; refusing here
            // is what keeps a dead resident from quietly becoming per-call
            // loading with no record that anything changed.
            return Ok(ResidencyOutcome::NotResident {
                reason: reason.clone(),
            });
        }
        if let Some(resident) = self.residents.get_mut(resident_key) {
            self.uses += 1;
            resident.last_used = self.uses;
            return Ok(ResidencyOutcome::Warm {
                server_identity: resident.server_identity.clone(),
                base_url: resident.spec.base_url.clone(),
            });
        }

        let spec = match Self::launch_spec(resident_key, &entry, registry) {
            Ok(spec) => spec,
            Err(reason) => return Ok(ResidencyOutcome::NotResident { reason }),
        };

        // PRD-047/PRD-062 gate: residency never loads a model the artifact
        // registry does not know.
        if ModelArtifactRepository::new(db)
            .get(&spec.model_artifact_id)?
            .is_none()
        {
            return Ok(ResidencyOutcome::NotResident {
                reason: format!(
                    "{}:{}",
                    reasons::ARTIFACT_NOT_REGISTERED,
                    spec.model_artifact_id
                ),
            });
        }

        if let Some(reason) = self.make_room_for(db, &spec)? {
            return Ok(ResidencyOutcome::NotResident { reason });
        }

        let server_identity = self.next_server_identity(resident_key);
        match self.start(db, &spec, &server_identity) {
            Ok(()) => {
                self.uses += 1;
                self.failure_attempts.remove(resident_key);
                self.residents.insert(
                    resident_key.to_string(),
                    Resident {
                        spec: spec.clone(),
                        server_identity: server_identity.clone(),
                        last_used: self.uses,
                    },
                );
                Ok(ResidencyOutcome::Cold {
                    server_identity,
                    base_url: spec.base_url,
                })
            }
            Err(reason) => {
                let attempts = self
                    .failure_attempts
                    .entry(resident_key.to_string())
                    .or_insert(0);
                *attempts += 1;
                let exhausted = *attempts > self.config.max_restarts;
                self.record(
                    db,
                    &spec,
                    &server_identity,
                    ResidencyEvent::HealthFailed,
                    Some(&reason),
                    None,
                )?;
                if exhausted {
                    self.record(
                        db,
                        &spec,
                        &server_identity,
                        ResidencyEvent::Failed,
                        Some(reasons::RESTARTS_EXHAUSTED),
                        None,
                    )?;
                    self.failed.insert(
                        resident_key.to_string(),
                        format!("{}:{}", reasons::RESTARTS_EXHAUSTED, reason),
                    );
                }
                Ok(ResidencyOutcome::NotResident { reason })
            }
        }
    }

    /// Launches and waits for the server to answer, recording the load only
    /// once it actually serves.
    fn start(
        &self,
        db: &Database,
        spec: &ResidentLaunchSpec,
        server_identity: &str,
    ) -> Result<(), String> {
        self.launcher
            .launch(spec, server_identity)
            .map_err(|error| format!("{}:{error}", reasons::LAUNCH_FAILED))?;
        let deadline = Instant::now() + Duration::from_secs(spec.ready_timeout_secs);
        loop {
            match self.launcher.probe(server_identity, &spec.base_url) {
                Ok(()) => break,
                Err(error) => {
                    if Instant::now() >= deadline {
                        let _ = self.launcher.stop(server_identity);
                        return Err(format!("{}:{error}", reasons::READY_TIMEOUT));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        self.record(
            db,
            spec,
            server_identity,
            ResidencyEvent::Loaded,
            None,
            None,
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Evicts least-recently-used residents until `candidate` fits both
    /// ceilings. Returns a refusal reason when it cannot fit at all.
    fn make_room_for(
        &mut self,
        db: &Database,
        candidate: &ResidentLaunchSpec,
    ) -> familiar_ai_core::Result<Option<String>> {
        if let (Some(ceiling), Some(memory)) = (self.config.memory_ceiling_mb, candidate.memory_mb)
        {
            if memory > ceiling {
                // No amount of eviction makes this fit; say so rather than
                // evicting everything and then failing anyway.
                return Ok(Some(format!(
                    "{}:{memory}mb>{ceiling}mb",
                    reasons::EXCEEDS_MEMORY_CEILING
                )));
            }
        }
        loop {
            let Some(ceiling) = self.breached_ceiling(candidate) else {
                return Ok(None);
            };
            let Some(victim) = self.least_recently_used() else {
                return Ok(Some(format!(
                    "{}:{}",
                    reasons::EXCEEDS_MEMORY_CEILING,
                    ceiling.as_str()
                )));
            };
            self.evict(db, &victim, ceiling.as_str())?;
        }
    }

    /// Which ceiling admitting `candidate` alongside the current residents
    /// would breach, if any.
    fn breached_ceiling(&self, candidate: &ResidentLaunchSpec) -> Option<ResidencyCeiling> {
        if self.residents.len() as u32 + 1 > self.config.max_residents {
            return Some(ResidencyCeiling::Count);
        }
        if let Some(ceiling) = self.config.memory_ceiling_mb {
            let held: u64 = self
                .residents
                .values()
                .filter_map(|resident| resident.spec.memory_mb)
                .sum();
            if held + candidate.memory_mb.unwrap_or(0) > ceiling {
                return Some(ResidencyCeiling::Memory);
            }
        }
        None
    }

    fn least_recently_used(&self) -> Option<String> {
        self.residents
            .iter()
            .min_by_key(|(_, resident)| resident.last_used)
            .map(|(key, _)| key.clone())
    }

    fn evict(
        &mut self,
        db: &Database,
        resident_key: &str,
        reason: &str,
    ) -> familiar_ai_core::Result<()> {
        let Some(resident) = self.residents.remove(resident_key) else {
            return Ok(());
        };
        let _ = self.launcher.stop(&resident.server_identity);
        self.record(
            db,
            &resident.spec,
            &resident.server_identity,
            ResidencyEvent::Evicted,
            Some(reason),
            None,
        )?;
        Ok(())
    }

    /// Stops one resident and records it.
    pub fn stop(
        &mut self,
        db: &Database,
        resident_key: &str,
        reason: &str,
    ) -> familiar_ai_core::Result<bool> {
        let Some(resident) = self.residents.remove(resident_key) else {
            return Ok(false);
        };
        let _ = self.launcher.stop(&resident.server_identity);
        self.record(
            db,
            &resident.spec,
            &resident.server_identity,
            ResidencyEvent::Stopped,
            Some(reason),
            None,
        )?;
        Ok(true)
    }

    pub fn stop_all(&mut self, db: &Database, reason: &str) -> familiar_ai_core::Result<usize> {
        let mut stopped = 0;
        for key in self.resident_keys() {
            if self.stop(db, &key, reason)? {
                stopped += 1;
            }
        }
        Ok(stopped)
    }

    /// Stops residents the current configuration no longer enables. This is
    /// how `model-residency disable` takes effect: the CLI writes the
    /// audited configuration change, and the daemon that actually owns the
    /// process stops it here and records the stop.
    pub fn reconcile(&mut self, db: &Database) -> familiar_ai_core::Result<usize> {
        let stale: Vec<String> = self
            .residents
            .keys()
            .filter(|key| !self.config.is_active(key))
            .cloned()
            .collect();
        let mut stopped = 0;
        for key in stale {
            if self.stop(db, &key, reasons::DISABLED_BY_CONFIGURATION)? {
                stopped += 1;
            }
        }
        Ok(stopped)
    }

    /// One health interval: reconcile configuration, then probe every
    /// resident, restarting within budget and failing loudly past it.
    pub fn health_tick(&mut self, db: &Database) -> familiar_ai_core::Result<()> {
        self.reconcile(db)?;
        for key in self.resident_keys() {
            let Some(resident) = self.residents.get(&key) else {
                continue;
            };
            let spec = resident.spec.clone();
            let server_identity = resident.server_identity.clone();
            match self.launcher.probe(&server_identity, &spec.base_url) {
                // A healthy resident writes nothing. This table records what
                // the daemon did and what went wrong; a row per healthy
                // interval would bury exactly those events.
                Ok(()) => {}
                Err(error) => {
                    let reason = format!("{}:{error}", reasons::HEALTH_PROBE_FAILED);
                    self.record(
                        db,
                        &spec,
                        &server_identity,
                        ResidencyEvent::HealthFailed,
                        Some(&reason),
                        None,
                    )?;
                    self.restart_or_fail(db, &key, &spec, &server_identity)?;
                }
            }
        }
        Ok(())
    }

    fn restart_or_fail(
        &mut self,
        db: &Database,
        resident_key: &str,
        spec: &ResidentLaunchSpec,
        server_identity: &str,
    ) -> familiar_ai_core::Result<()> {
        let attempts = *self.failure_attempts.get(resident_key).unwrap_or(&0);
        if attempts >= self.config.max_restarts {
            let _ = self.launcher.stop(server_identity);
            self.residents.remove(resident_key);
            self.record(
                db,
                spec,
                server_identity,
                ResidencyEvent::Failed,
                Some(reasons::RESTARTS_EXHAUSTED),
                None,
            )?;
            self.failed.insert(
                resident_key.to_string(),
                format!("{}:{resident_key}", reasons::RESTARTS_EXHAUSTED),
            );
            return Ok(());
        }
        let attempt = attempts + 1;
        self.failure_attempts
            .insert(resident_key.to_string(), attempt);
        let _ = self.launcher.stop(server_identity);
        let replacement = self.next_server_identity(resident_key);
        match self.start(db, spec, &replacement) {
            Ok(()) => {
                self.uses += 1;
                let uses = self.uses;
                if let Some(resident) = self.residents.get_mut(resident_key) {
                    resident.server_identity = replacement.clone();
                    resident.last_used = uses;
                }
                self.record(
                    db,
                    spec,
                    &replacement,
                    ResidencyEvent::Restarted,
                    Some(reasons::HEALTH_PROBE_FAILED),
                    Some(attempt),
                )?;
                Ok(())
            }
            Err(error) => {
                let reason = format!("restart-{}:{error}", reasons::LAUNCH_FAILED);
                self.record(
                    db,
                    spec,
                    &replacement,
                    ResidencyEvent::HealthFailed,
                    Some(&reason),
                    None,
                )?;
                if attempt >= self.config.max_restarts {
                    self.residents.remove(resident_key);
                    self.record(
                        db,
                        spec,
                        &replacement,
                        ResidencyEvent::Failed,
                        Some(reasons::RESTARTS_EXHAUSTED),
                        None,
                    )?;
                    self.failed.insert(
                        resident_key.to_string(),
                        format!("{}:{resident_key}", reasons::RESTARTS_EXHAUSTED),
                    );
                }
                Ok(())
            }
        }
    }
}

/// The production launcher: spawns the operator's declared argv, kills it on
/// stop, and probes it over PRD-063's own OpenAI-compatible model listing.
///
/// The launch command is never derived from the runtime kind. Familiar does
/// not know how a given installation starts `ollama` or `unsloth`, and
/// guessing would fabricate operator intent, so the argv comes from
/// configuration and is executed directly — no shell, no interpolation.
pub struct ProcessResidentLauncher {
    children: Mutex<HashMap<String, Child>>,
    probe_timeout: Duration,
}

impl ProcessResidentLauncher {
    pub fn new(probe_timeout_secs: u64) -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
            probe_timeout: Duration::from_secs(probe_timeout_secs.max(1)),
        }
    }
}

impl Default for ProcessResidentLauncher {
    fn default() -> Self {
        Self::new(5)
    }
}

impl ResidentServerLauncher for ProcessResidentLauncher {
    fn launch(&self, spec: &ResidentLaunchSpec, server_identity: &str) -> Result<(), String> {
        let program = spec
            .launch
            .first()
            .ok_or("resident launch command is empty")?;
        let child = Command::new(program)
            .args(&spec.launch[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot start resident '{program}': {error}"))?;
        self.children
            .lock()
            .map_err(|_| "residency child table poisoned".to_string())?
            .insert(server_identity.to_string(), child);
        Ok(())
    }

    fn stop(&self, server_identity: &str) -> Result<(), String> {
        let child = self
            .children
            .lock()
            .map_err(|_| "residency child table poisoned".to_string())?
            .remove(server_identity);
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    fn probe(&self, server_identity: &str, base_url: &str) -> Result<(), String> {
        {
            let mut children = self
                .children
                .lock()
                .map_err(|_| "residency child table poisoned".to_string())?;
            match children.get_mut(server_identity) {
                Some(child) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        return Err(format!("resident process exited with {status}"));
                    }
                }
                None => return Err("resident process is not running".into()),
            }
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(self.probe_timeout)
            .build()
            .map_err(|error| format!("cannot build residency probe client: {error}"))?;
        let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
        let response = client
            .get(&url)
            .send()
            .map_err(|error| format!("resident endpoint unreachable: {error}"))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "resident endpoint answered {}",
                response.status().as_u16()
            ))
        }
    }
}

/// The daemon's residency task. Exits immediately when residency is off, so
/// spawning it unconditionally costs a disabled workspace nothing.
pub async fn run(
    db: Arc<std::sync::Mutex<Database>>,
    manager: Arc<std::sync::Mutex<ResidencyManager>>,
    config: Config,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    if !config.model_residency.enabled {
        tracing::info!("model residency disabled; no local server will be held resident");
        return;
    }
    let interval = Duration::from_secs(config.model_residency.health_interval_secs.max(1));
    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                let db = db.clone();
                let manager = manager.clone();
                let stopped = tokio::task::spawn_blocking(move || {
                    let db = db.lock().expect("residency database lock");
                    let mut manager = manager.lock().expect("residency manager lock");
                    manager.stop_all(&db, reasons::DAEMON_SHUTDOWN)
                })
                .await;
                match stopped {
                    Ok(Ok(count)) => tracing::info!(residents = count, "model residency stopped"),
                    Ok(Err(error)) => tracing::error!(error = %error, "model residency shutdown failed"),
                    Err(error) => tracing::error!(error = %error, "model residency shutdown task failed"),
                }
                return;
            }
            _ = ticker.tick() => {
                let db = db.clone();
                let manager = manager.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let db = db.lock().expect("residency database lock");
                    let mut manager = manager.lock().expect("residency manager lock");
                    manager.health_tick(&db)
                })
                .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::error!(error = %error, "model residency health check failed"),
                    Err(error) => tracing::error!(error = %error, "model residency health task failed"),
                }
            }
        }
    }
}
