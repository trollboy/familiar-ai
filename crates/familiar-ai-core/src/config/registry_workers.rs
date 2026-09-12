use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{validate_identifier, validate_model_identifier, ReviewConfig};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentAdapterKind {
    #[default]
    Codex,
    ClaudeCode,
    /// Ollama is invoked through the existing Codex OSS adapter.
    Ollama,
}

impl AgentAdapterKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::Ollama => "ollama",
        }
    }
    pub fn default_executable(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
            Self::Ollama => "codex",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentEffort {
    Low,
    Medium,
    High,
}

impl AgentEffort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentPermissionMode {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    #[serde(rename = "bypassPermissions")]
    BypassPermissions,
}

impl AgentPermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPermissions => "bypassPermissions",
        }
    }
}

/// Flags the adapter owns or forbids; configured `extra_args` may not name
/// them, exactly or in `<flag>=value` form.
pub const FORBIDDEN_AGENT_EXTRA_ARGS: [&str; 11] = [
    "--print",
    "--output-format",
    "--input-format",
    "--verbose",
    "--model",
    "--permission-mode",
    "--resume",
    "--continue",
    "--session-id",
    "--fork-session",
    "--dangerously-skip-permissions",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEntryConfig {
    #[serde(default)]
    pub adapter: AgentAdapterKind,
    #[serde(default)]
    pub executable: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<AgentEffort>,
    #[serde(default)]
    pub permission_mode: Option<AgentPermissionMode>,
    /// 0 or absent means no per-execution cost ceiling.
    #[serde(default)]
    #[serde(alias = "max_budget_microusd")]
    pub max_execution_cost_microusd: u64,
    #[serde(default)]
    pub max_execution_tokens: u64,
    #[serde(default)]
    pub max_execution_duration_ms: u64,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl AgentEntryConfig {
    pub fn resolved_executable(&self) -> String {
        self.executable
            .clone()
            .unwrap_or_else(|| self.adapter.default_executable().to_owned())
    }

    fn validate(&self, role: &str, is_reviewer: bool) -> Result<(), String> {
        if let Some(executable) = &self.executable {
            if executable.trim().is_empty() {
                return Err(format!("[agents.{role}] executable must be non-empty"));
            }
        }
        if matches!(
            self.adapter,
            AgentAdapterKind::Codex | AgentAdapterKind::Ollama
        ) && (self.effort.is_some() || self.permission_mode.is_some())
        {
            return Err(format!(
                "[agents.{role}] effort and permission_mode are valid only for adapter \"claude-code\""
            ));
        }
        if is_reviewer && self.permission_mode == Some(AgentPermissionMode::BypassPermissions) {
            return Err(format!(
                "[agents.{role}] bypassPermissions is never permitted for the reviewer"
            ));
        }
        for arg in &self.extra_args {
            for flag in FORBIDDEN_AGENT_EXTRA_ARGS {
                if arg == flag || arg.starts_with(&format!("{flag}=")) {
                    return Err(format!(
                        "[agents.{role}] extra_args may not include adapter-owned or forbidden flag '{flag}'"
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlannerConfig {
    #[serde(flatten)]
    pub agent: AgentEntryConfig,
    #[serde(default = "default_planner_max_prds")]
    pub max_prds_per_batch: usize,
    #[serde(default = "default_planner_max_bytes")]
    pub max_bytes_per_prd: usize,
}

const fn default_planner_max_prds() -> usize {
    8
}

const fn default_planner_max_bytes() -> usize {
    64 * 1024
}

impl PlannerConfig {
    pub fn validate(&self) -> Result<(), String> {
        self.agent.validate("planner", false)?;
        if self.max_prds_per_batch == 0 || self.max_bytes_per_prd == 0 {
            return Err(
                "[planner] max_prds_per_batch and max_bytes_per_prd must be positive and finite"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerCapabilityConfig {
    Planning,
    Implementation,
    Review,
    Remediation,
    NarrowTask,
}

/// Closed routing/runtime capability vocabulary. Stage eligibility remains a
/// separate concern and is migrated from the historical `capabilities` list.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeCapabilityConfig {
    EditsFiles,
    ExecutesCommands,
    ReadsRepository,
    NativeToolCalling,
    McpClient,
    StructuredOutput,
    Streaming,
    ResumableSessions,
    ContextCompaction,
    PromptCaching,
    ImageInput,
    MaxContext,
    ReasoningControls,
    SandboxBehavior,
    RemoteOrLocal,
    UsageReportingCategories,
    CostReportingMode,
    ParallelToolCalls,
    DeterministicSeed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityProvenanceConfig {
    Declared,
    Probed,
    Observed,
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityProfileConfig {
    #[serde(default)]
    pub capabilities: BTreeMap<RuntimeCapabilityConfig, CapabilityProvenanceConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OllamaRuntimeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// PRD-063 local worker `RuntimeId` vocabulary. A local worker is
/// `provider = "local"` plus one of these runtimes; the same artifact under
/// two different runtimes is two distinct workers for routing, telemetry,
/// and empirical history (PRD-057 discipline extended to local hardware).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LocalRuntimeKind {
    Unsloth,
    Ollama,
}

impl LocalRuntimeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsloth => "unsloth",
            Self::Ollama => "ollama",
        }
    }

    /// Whether this runtime exposes the metadata/digests needed to verify a
    /// serving endpoint's claimed model against a registered PRD-062
    /// artifact. A runtime without this stays degraded-unverified rather
    /// than ever inferring a match from the model name alone.
    pub const fn supports_artifact_digest(self) -> bool {
        matches!(self, Self::Ollama)
    }
}

/// Endpoint trust is explicit and derived, never operator-declared, so it
/// can never silently drift from the endpoint it describes (PRD-063).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LocalEndpointTrustConfig {
    Loopback,
    Lan,
    Remote,
}

fn extract_host(base_url: &str) -> &str {
    let without_scheme = base_url.split("://").nth(1).unwrap_or(base_url);
    let authority = without_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    }
}

/// Classifies a local endpoint's trust class straight from its URL host:
/// loopback, private/link-local (LAN), or everything else (Remote — the
/// stricter default for anything Familiar cannot positively classify as
/// private).
pub fn classify_local_endpoint_trust(base_url: &str) -> LocalEndpointTrustConfig {
    let host = extract_host(base_url);
    if host.eq_ignore_ascii_case("localhost") || host == "::1" {
        return LocalEndpointTrustConfig::Loopback;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) if v4.octets()[0] == 127 => LocalEndpointTrustConfig::Loopback,
        Ok(std::net::IpAddr::V4(v4)) => {
            let o = v4.octets();
            if o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 169 && o[1] == 254)
            {
                LocalEndpointTrustConfig::Lan
            } else {
                LocalEndpointTrustConfig::Remote
            }
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            let first = v6.segments()[0];
            if v6.is_loopback() {
                LocalEndpointTrustConfig::Loopback
            } else if (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 {
                LocalEndpointTrustConfig::Lan
            } else {
                LocalEndpointTrustConfig::Remote
            }
        }
        Err(_) => LocalEndpointTrustConfig::Remote,
    }
}

/// The PRD-063 endpoint profile: where the runtime is reached and how it is
/// trusted. Authentication, when required, is the generic
/// `RegistryWorkerConfig.auth_profile` BYO-Auth reference shared by every
/// worker kind — never a duplicate credential surface here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalEndpointConfig {
    pub base_url: String,
    #[serde(default)]
    pub tls: bool,
}

impl LocalEndpointConfig {
    pub fn trust(&self) -> LocalEndpointTrustConfig {
        classify_local_endpoint_trust(&self.base_url)
    }

    /// Whether `base_url`'s scheme is actually `https`. `tls` is validated
    /// against this rather than trusted as an independent operator
    /// declaration — a mismatched declaration is a configuration error, not
    /// silently ignored.
    pub fn base_url_uses_https(&self) -> bool {
        self.base_url
            .split("://")
            .next()
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https"))
    }
}

/// The PRD-063 hardware/resource profile: what PRD-064 typed reservations
/// local execution must acquire before running. Every field absent means
/// unmeasured/unbounded-by-Familiar, never zero or unlimited by assumption;
/// absent thermal/power ceilings mean the platform exposes no such sensor
/// (recorded as unknown, never assumed safe).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct LocalResourceProfileConfig {
    pub accelerator_memory_mb: Option<u64>,
    pub system_memory_mb: Option<u64>,
    pub concurrent_inference_slots: Option<u64>,
    pub model_loading_slots: Option<u64>,
    pub exclusive_runtime: bool,
    pub thermal_ceiling_celsius: Option<u64>,
    pub power_ceiling_watts: Option<u64>,
}

/// The complete PRD-063 local addition to a PRD-057 worker spec: which
/// runtime, where it is reached, and what hardware it reserves. Present
/// only when `RegistryWorkerConfig.provider == LOCAL_PROVIDER`.
///
/// Declaring this block validates a worker spec and makes it addressable
/// (`familiar_ai_agent::local_worker::LocalInferenceAdapter`, the PRD-064
/// reservation glue in `familiar_ai_daemon::local_worker_runtime`, and the
/// PRD-051 telemetry sink all exist and are exercised end-to-end by their
/// own fake-endpoint test suites). It does **not** yet make the entry
/// reachable from `familiar_ai_daemon::run`'s worker-selection/execution
/// path: that path (`build_agent`/`AdapterFactories`, keyed by
/// `familiar_ai_core::config::AgentAdapterKind`) only knows the CLI-driven
/// adapters (`codex`, `claude-code`, `ollama`-via-Codex-harness). Routing a
/// `provider = "local"` entry into a real execution is deferred to a
/// follow-up change, matching the identical, pre-existing state of every
/// other PRD-058 raw-runtime adapter in this workspace (Anthropic, OpenAI,
/// xAI: fully implemented and tested, none reachable from production
/// dispatch either). Until that follow-up lands,
/// `familiar_ai_daemon::run::resolved_worker_plan` fails closed rather than
/// silently misdispatching: a selected worker declaring this block returns
/// an explicit `Err` before any `CodingAgent` is built, because its
/// `runtime` (e.g. `"ollama"`) can otherwise collide with an unrelated
/// pre-existing CLI-driven adapter id of the same name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalWorkerConfig {
    pub runtime_kind: LocalRuntimeKind,
    pub endpoint: LocalEndpointConfig,
    #[serde(default)]
    pub resources: LocalResourceProfileConfig,
}

/// The `provider` value every PRD-063 local worker uses. Distinct from
/// which `[providers.<name>]` inference entry (if any) discovered the
/// endpoint — a local worker's routing/telemetry identity is its full
/// spec, not a provider directory entry.
pub const LOCAL_PROVIDER: &str = "local";

/// PRD-073 warm local model residency: which PRD-062 artifacts the PRD-056
/// daemon may keep loaded between executions, and the ceilings that bound
/// them.
///
/// Residency is **off by default** in two independent places — the global
/// `enabled` flag and each resident's own `enabled` flag — because holding
/// gigabytes of weights resident is an operator resource commitment, never
/// something Familiar infers from a worker merely existing. A declared but
/// disabled resident is configuration, not activation (the same discipline
/// PRD-063's `local_allocation_policies.enabled` applies to cost).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct ModelResidencyConfig {
    /// The master switch. False means the daemon starts no resident server,
    /// holds none, and routes exactly as PRD-063 does today.
    pub enabled: bool,
    /// How many residents may be held at once. LRU eviction fires at this
    /// ceiling.
    pub max_residents: u32,
    /// Total declared resident memory ceiling. Absent means only
    /// `max_residents` bounds residency — never an assumed byte budget.
    pub memory_ceiling_mb: Option<u64>,
    /// How often the daemon probes each resident for liveness.
    pub health_interval_secs: u64,
    /// How many times one resident may be restarted after a failed health
    /// probe before it is marked failed with a named reason.
    pub max_restarts: u32,
    /// Declared residents, keyed by an operator-chosen resident key.
    pub residents: BTreeMap<String, ResidentModelConfig>,
}

impl Default for ModelResidencyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_residents: 1,
            memory_ceiling_mb: None,
            health_interval_secs: 30,
            max_restarts: 2,
            residents: BTreeMap::new(),
        }
    }
}

/// One declared resident: a `worker_registry.workers` entry that must carry
/// a PRD-063 `local` profile, plus the exact command that serves it.
///
/// `launch` is an explicit operator declaration and never derived from
/// `runtime_kind`. Familiar does not know how any given installation starts
/// `ollama` or `unsloth` — guessing a command line would fabricate operator
/// intent and break silently whenever a serving runtime changes its CLI, so
/// an empty `launch` is a configuration error rather than a default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct ResidentModelConfig {
    /// Per-artifact opt-in. Declaring the entry is not enabling it.
    pub enabled: bool,
    /// The `worker_registry.workers` key this resident serves.
    pub worker: String,
    /// Program and arguments, argv-style. Never shell-interpreted.
    pub launch: Vec<String>,
    /// This resident's declared memory footprint. Required whenever
    /// `memory_ceiling_mb` is set: a resident of unknown size cannot be
    /// admitted against a byte budget without inventing its size.
    pub memory_mb: Option<u64>,
    /// How long to wait for a freshly launched server to answer a health
    /// probe before the launch is a recorded failure.
    pub ready_timeout_secs: u64,
}

impl Default for ResidentModelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            worker: String::new(),
            launch: Vec::new(),
            memory_mb: None,
            ready_timeout_secs: 60,
        }
    }
}

/// The residency ceiling a candidate would breach, named so the daemon's
/// eviction record and the operator's error message use one vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyCeiling {
    Count,
    Memory,
}

impl ResidencyCeiling {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Count => "count-ceiling",
            Self::Memory => "memory-ceiling",
        }
    }
}

impl ModelResidencyConfig {
    /// The residents actually eligible to be held: the global switch and the
    /// resident's own switch must both be on.
    pub fn active_residents(&self) -> impl Iterator<Item = (&String, &ResidentModelConfig)> {
        let enabled = self.enabled;
        self.residents
            .iter()
            .filter(move |(_, resident)| enabled && resident.enabled)
    }

    pub fn is_active(&self, key: &str) -> bool {
        self.enabled && self.residents.get(key).is_some_and(|entry| entry.enabled)
    }

    /// Validates residency against the worker registry it references. A
    /// resident naming an absent worker, a worker with no PRD-063 `local`
    /// profile, or a worker with no PRD-062 `model_artifact` is refused:
    /// residency must never load something the artifact registry does not
    /// know (PRD-047/PRD-062 enablement).
    pub fn validate(&self, registry: Option<&WorkerRegistryConfig>) -> Result<(), String> {
        if self.enabled && self.max_residents == 0 {
            return Err(
                "model_residency.max_residents must be at least 1 when residency is enabled".into(),
            );
        }
        if self.enabled && self.health_interval_secs == 0 {
            return Err("model_residency.health_interval_secs must be greater than zero".into());
        }
        for (key, resident) in &self.residents {
            validate_identifier("model_residency.residents", key)?;
            if resident.worker.trim().is_empty() {
                return Err(format!(
                    "model_residency.residents.{key} must name a worker_registry.workers entry"
                ));
            }
            if resident.launch.is_empty() || resident.launch[0].trim().is_empty() {
                return Err(format!(
                    "model_residency.residents.{key}.launch must declare the serving command; \
                     Familiar never guesses how a local runtime is started"
                ));
            }
            if resident.ready_timeout_secs == 0 {
                return Err(format!(
                    "model_residency.residents.{key}.ready_timeout_secs must be greater than zero"
                ));
            }
            if self.memory_ceiling_mb.is_some() && resident.memory_mb.is_none() {
                return Err(format!(
                    "model_residency.residents.{key}.memory_mb is required while \
                     model_residency.memory_ceiling_mb is set: a resident of unknown \
                     size cannot be admitted against a byte budget"
                ));
            }
            let worker = registry
                .and_then(|registry| registry.workers.get(&resident.worker))
                .ok_or_else(|| {
                    format!(
                        "model_residency.residents.{key}.worker '{}' is not a configured \
                         worker_registry.workers entry",
                        resident.worker
                    )
                })?;
            if worker.local.is_none() {
                return Err(format!(
                    "model_residency.residents.{key}.worker '{}' declares no PRD-063 \
                     local profile; only a local serving endpoint can be held resident",
                    resident.worker
                ));
            }
            if !worker
                .model_artifact
                .as_ref()
                .is_some_and(|artifact| !artifact.trim().is_empty())
            {
                return Err(format!(
                    "model_residency.residents.{key}.worker '{}' declares no model_artifact; \
                     residency never loads a model outside the PRD-062 artifact registry",
                    resident.worker
                ));
            }
        }
        Ok(())
    }
}

impl WorkerCapabilityConfig {
    /// The canonical serialized spelling — identical to the serde kebab-case
    /// form and to what the CLI accepts, so display output always round-trips
    /// (FAM-FRICTION-004: `NarrowTask` must render as `narrow-task`, never
    /// `narrowtask`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Implementation => "implementation",
            Self::Review => "review",
            Self::Remediation => "remediation",
            Self::NarrowTask => "narrow-task",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegistryWorkerConfig {
    /// Compatibility input for PRD-031 registries. New entries use `runtime`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<AgentAdapterKind>,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_profile: Option<String>,
    /// The only runtime extension introduced in this PRD. Other runtimes must
    /// add their own closed adapter-owned type when their adapter is added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_config: Option<OllamaRuntimeConfig>,
    /// The PRD-063 local worker endpoint/hardware profile. Present only for
    /// `provider = "local"` workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalWorkerConfig>,
    #[serde(default)]
    pub executable: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<WorkerCapabilityConfig>,
    #[serde(default)]
    pub fresh_process_isolation: bool,
    #[serde(default)]
    pub context_tokens: u64,
    /// Absent means never measured. An operator who has not measured a
    /// worker's cost must not have it silently treated as free
    /// (FAM-BUG-007).
    #[serde(default)]
    pub estimated_cost_microusd: Option<u64>,
    #[serde(default = "default_worker_available")]
    pub available: bool,
    #[serde(default)]
    pub effort: Option<AgentEffort>,
    #[serde(default)]
    pub permission_mode: Option<AgentPermissionMode>,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

fn default_worker_available() -> bool {
    true
}

impl RegistryWorkerConfig {
    pub fn runtime_id(&self) -> Result<&str, String> {
        match (&self.runtime, self.adapter) {
            (Some(runtime), Some(adapter)) if runtime != adapter.as_str() => Err(format!(
                "runtime '{runtime}' contradicts legacy adapter '{}'",
                adapter.as_str()
            )),
            (Some(runtime), _) => Ok(runtime),
            (None, Some(adapter)) => Ok(adapter.as_str()),
            (None, None) => Err("requires runtime (or legacy adapter)".into()),
        }
    }

    pub fn canonical_spec_identity(&self) -> Result<String, String> {
        let runtime = self.runtime_id()?;
        let model = if self.model.is_empty() {
            "unknown"
        } else {
            &self.model
        };
        let artifact = self.model_artifact.as_deref().unwrap_or("-");
        let profile = self.capability_profile.as_deref().unwrap_or("legacy");
        let material = format!(
            "provider={}\nruntime={}\nmodel={}\nartifact={}\ncapability-profile={}",
            self.provider, runtime, model, artifact, profile
        );
        let hash = ring::digest::digest(&ring::digest::SHA256, material.as_bytes());
        Ok(format!(
            "wspec-sha256:{}",
            hash.as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ))
    }

    pub fn empirical_version(&self) -> Result<String, String> {
        let material = serde_json::to_vec(&(
            self.canonical_spec_identity()?,
            self.model_artifact.as_deref(),
            self.effort,
            self.permission_mode,
            self.context_tokens,
            &self.extra_args,
        ))
        .map_err(|error| error.to_string())?;
        let hash = ring::digest::digest(&ring::digest::SHA256, &material);
        Ok(format!(
            "wver-sha256:{}",
            hash.as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ))
    }

    pub fn as_agent_entry(&self) -> AgentEntryConfig {
        let adapter = self.adapter.unwrap_or(match self.runtime.as_deref() {
            Some("claude-code") => AgentAdapterKind::ClaudeCode,
            Some("ollama") => AgentAdapterKind::Ollama,
            _ => AgentAdapterKind::Codex,
        });
        let model = if self.model == "__legacy_cli_default__" {
            None
        } else {
            match adapter {
                AgentAdapterKind::Ollama if !self.model.starts_with("ollama/") => {
                    Some(format!("ollama/{}", self.model))
                }
                _ => Some(self.model.clone()),
            }
        };
        AgentEntryConfig {
            adapter,
            executable: self.executable.clone(),
            model,
            effort: self.effort,
            permission_mode: self.permission_mode,
            extra_args: self.extra_args.clone(),
            ..AgentEntryConfig::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerRoutingConfig {
    #[serde(default)]
    pub implementation_pin: Option<String>,
    #[serde(default)]
    pub planning_pin: Option<String>,
    #[serde(default)]
    pub review_pin: Option<String>,
    #[serde(default)]
    pub remediation_pin: Option<String>,
    #[serde(default)]
    pub narrow_task_pin: Option<String>,
    #[serde(default)]
    pub max_stage_cost_microusd: u64,
    #[serde(default)]
    pub required_context_tokens: u64,
    /// Operator-authored route rules. First match wins, mirroring review tier
    /// rules; applied ahead of the lowest-cost-then-id tiebreak.
    #[serde(default)]
    pub rules: Vec<WorkerRouteRuleConfig>,
}

/// Selects a worker by declared risk and expected scope size. Absent
/// `risk_classes` and `max_expected_files` predicates are unconstrained; a
/// rule must declare at least one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerRouteRuleConfig {
    pub id: String,
    pub worker: String,
    #[serde(default)]
    pub risk_classes: Vec<String>,
    #[serde(default)]
    pub max_expected_files: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerRegistryConfig {
    #[serde(default)]
    pub workers: BTreeMap<String, RegistryWorkerConfig>,
    #[serde(default)]
    pub capability_profiles: BTreeMap<String, CapabilityProfileConfig>,
    #[serde(default)]
    pub routing: WorkerRoutingConfig,
}

impl WorkerRegistryConfig {
    /// Losslessly represent the legacy role configuration in the registry.
    /// Explicit pins preserve legacy selection semantics for all three stages.
    pub fn from_legacy_agents(agents: &AgentsConfig) -> Self {
        let worker = |entry: &AgentEntryConfig, role: &str, capabilities| RegistryWorkerConfig {
            adapter: Some(entry.adapter),
            // Keep role identities independent even when both legacy entries
            // use the same adapter and model.
            provider: format!("legacy-{}-{role}", entry.adapter.as_str()),
            model: entry
                .model
                .clone()
                .unwrap_or_else(|| "__legacy_cli_default__".to_owned()),
            runtime: Some(entry.adapter.as_str().to_owned()),
            model_artifact: None,
            auth_profile: None,
            capability_profile: None,
            runtime_config: None,
            local: None,
            executable: entry.executable.clone(),
            capabilities,
            fresh_process_isolation: true,
            context_tokens: 0,
            estimated_cost_microusd: None,
            available: true,
            effort: entry.effort,
            permission_mode: entry.permission_mode,
            extra_args: entry.extra_args.clone(),
        };
        Self {
            workers: BTreeMap::from([
                (
                    "legacy-implementation".to_owned(),
                    worker(
                        &agents.implementation,
                        "implementation",
                        vec![
                            WorkerCapabilityConfig::Implementation,
                            WorkerCapabilityConfig::Remediation,
                        ],
                    ),
                ),
                (
                    "legacy-reviewer".to_owned(),
                    worker(
                        &agents.reviewer,
                        "reviewer",
                        vec![WorkerCapabilityConfig::Review],
                    ),
                ),
            ]),
            capability_profiles: BTreeMap::new(),
            routing: WorkerRoutingConfig {
                implementation_pin: Some("legacy-implementation".to_owned()),
                review_pin: Some("legacy-reviewer".to_owned()),
                remediation_pin: Some("legacy-implementation".to_owned()),
                ..WorkerRoutingConfig::default()
            },
        }
    }

    pub fn validate(
        &self,
        risk_vocabulary: &std::collections::BTreeSet<&str>,
    ) -> Result<(), String> {
        if self.workers.is_empty() {
            return Err("worker_registry.workers must not be empty".into());
        }
        for (id, worker) in &self.workers {
            if id.trim().is_empty()
                || worker.provider.trim().is_empty()
                || worker.model.trim().is_empty()
                || worker.capabilities.is_empty()
            {
                return Err(format!(
                    "worker_registry.workers.{id} requires provider, model, and capabilities"
                ));
            }
            validate_identifier(&worker.provider, "provider id")?;
            validate_identifier(
                worker
                    .runtime_id()
                    .map_err(|error| format!("worker_registry.workers.{id} {error}"))?,
                "runtime id",
            )?;
            if !matches!(worker.model.as_str(), "unknown" | "runtime-selected") {
                validate_model_identifier(&worker.model)?;
            }
            if let Some(artifact) = &worker.model_artifact {
                let valid = artifact.strip_prefix("sha256:").is_some_and(|hex| {
                    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
                });
                if !valid {
                    return Err(format!("worker_registry.workers.{id} model_artifact must be an immutable sha256 content identity"));
                }
            }
            if worker.runtime_config.is_some() && worker.runtime_id()? != "ollama" {
                return Err(format!("worker_registry.workers.{id}.runtime_config is owned only by the ollama adapter"));
            }
            if worker.local.is_some() && worker.runtime_config.is_some() {
                return Err(format!(
                    "worker_registry.workers.{id} local and runtime_config are mutually exclusive"
                ));
            }
            // `local` is the PRD-063 opt-in marker: declaring it requires
            // provider = "local" and internal consistency, but the reverse
            // is not required — `provider = "local"` is an ordinary free
            // string other worker kinds (e.g. a Codex-harness worker
            // labeled "local" for a cheap on-box model) may already use
            // for unrelated reasons, and this validation must not surprise
            // them.
            if let Some(local) = &worker.local {
                if worker.provider != LOCAL_PROVIDER {
                    return Err(format!(
                        "worker_registry.workers.{id} local requires provider = \"{LOCAL_PROVIDER}\""
                    ));
                }
                if worker.runtime_id()? != local.runtime_kind.as_str() {
                    return Err(format!(
                        "worker_registry.workers.{id} local.runtime_kind must match runtime"
                    ));
                }
                if local.endpoint.base_url.trim().is_empty() {
                    return Err(format!(
                        "worker_registry.workers.{id} local.endpoint.base_url must not be empty"
                    ));
                }
                if local.endpoint.trust() != LocalEndpointTrustConfig::Loopback {
                    if worker.auth_profile.is_none() {
                        return Err(format!(
                            "worker_registry.workers.{id} non-loopback local endpoints require auth_profile"
                        ));
                    }
                    // A credential is never sent in cleartext to a
                    // non-loopback host: both the declared `tls` flag and
                    // the URL's own scheme must say so, so the credential
                    // can never travel over plaintext by a mismatched or
                    // ignored declaration.
                    if !local.endpoint.tls || !local.endpoint.base_url_uses_https() {
                        return Err(format!(
                            "worker_registry.workers.{id} non-loopback local endpoints require tls = true and an https:// base_url"
                        ));
                    }
                } else if local.endpoint.tls && !local.endpoint.base_url_uses_https() {
                    return Err(format!(
                        "worker_registry.workers.{id} local.endpoint.tls = true requires an https:// base_url"
                    ));
                }
            }
            if let Some(profile) = &worker.capability_profile {
                if !self.capability_profiles.contains_key(profile) {
                    return Err(format!(
                        "worker_registry.workers.{id} names missing capability profile '{profile}'"
                    ));
                }
            }
            worker.as_agent_entry().validate(
                &format!("worker_registry.workers.{id}"),
                worker
                    .capabilities
                    .contains(&WorkerCapabilityConfig::Review),
            )?;
        }
        let mut rule_ids = std::collections::BTreeSet::new();
        let mut signatures = std::collections::BTreeMap::new();
        for rule in &self.routing.rules {
            if rule.id.trim().is_empty() || !rule_ids.insert(rule.id.clone()) {
                return Err(format!(
                    "worker_registry.routing.rules rule id '{}' is empty or duplicated",
                    rule.id
                ));
            }
            if !self.workers.contains_key(&rule.worker) {
                return Err(format!(
                    "worker_registry.routing.rules rule '{}' names unknown worker '{}'",
                    rule.id, rule.worker
                ));
            }
            if rule.risk_classes.is_empty() && rule.max_expected_files.is_none() {
                return Err(format!(
                    "worker_registry.routing.rules rule '{}' has no match predicates",
                    rule.id
                ));
            }
            let mut classes = rule.risk_classes.clone();
            classes.sort();
            classes.dedup();
            for class in &classes {
                if class.trim().is_empty() {
                    return Err(format!(
                        "worker_registry.routing.rules rule '{}' risk_classes entries must be non-empty",
                        rule.id
                    ));
                }
                if !risk_vocabulary.contains(class.as_str()) {
                    return Err(format!(
                        "worker_registry.routing.rules rule '{}' names risk class '{class}' outside the configured vocabulary",
                        rule.id
                    ));
                }
            }
            let signature = serde_json::to_string(&(classes, rule.max_expected_files))
                .map_err(|error| error.to_string())?;
            if let Some((other_id, other_worker)) =
                signatures.insert(signature, (rule.id.clone(), rule.worker.clone()))
            {
                if other_worker != rule.worker {
                    return Err(format!(
                        "worker_registry.routing.rules '{other_id}' and '{}' contradict each other",
                        rule.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// Resolve the historical provider/model address only when it has one
    /// possible runtime. Operator ids and canonical identities are exact.
    pub fn resolve_worker(&self, address: &str) -> Result<&RegistryWorkerConfig, String> {
        if let Some(worker) = self.workers.get(address) {
            return Ok(worker);
        }
        let candidates: Vec<_> = self
            .workers
            .iter()
            .filter(|(_, w)| format!("{}/{}", w.provider, w.model) == address)
            .collect();
        match candidates.as_slice() {
            [(_, worker)] => Ok(worker),
            [] => Err(format!("unknown worker '{address}'")),
            _ => Err(format!(
                "legacy worker alias '{address}' is ambiguous; use one of: {}",
                candidates
                    .iter()
                    .map(|(id, w)| format!(
                        "{} ({}/{}/{})",
                        id,
                        w.provider,
                        w.runtime_id().unwrap_or("invalid"),
                        w.model
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentsConfig {
    #[serde(default)]
    pub implementation: AgentEntryConfig,
    #[serde(default)]
    pub reviewer: AgentEntryConfig,
}

impl AgentsConfig {
    /// Fail-closed validation, including consistency with the authoritative
    /// review audit identities when review is enabled. Runs only when the
    /// `[agents]` section is present.
    pub fn validate(&self, review: &ReviewConfig) -> Result<(), String> {
        self.implementation.validate("implementation", false)?;
        self.reviewer.validate("reviewer", true)?;
        if review.enabled {
            for (role, entry, identity) in [
                (
                    "implementation",
                    &self.implementation,
                    &review.implementation_agent,
                ),
                ("reviewer", &self.reviewer, &review.reviewer_agent),
            ] {
                if identity.adapter_id != entry.adapter.as_str() {
                    return Err(format!(
                        "review.{role}_agent.adapter_id '{}' contradicts agents.{role}.adapter '{}'",
                        identity.adapter_id,
                        entry.adapter.as_str()
                    ));
                }
                if let (Some(review_model), Some(agent_model)) = (&identity.model, &entry.model) {
                    if review_model != agent_model {
                        return Err(format!(
                            "review.{role}_agent.model '{review_model}' contradicts agents.{role}.model '{agent_model}'"
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod local_worker_tests {
    use super::*;

    #[test]
    fn endpoint_trust_classifies_loopback_lan_and_remote() {
        for (url, expected) in [
            ("http://127.0.0.1:11434", LocalEndpointTrustConfig::Loopback),
            ("http://localhost:11434", LocalEndpointTrustConfig::Loopback),
            ("http://[::1]:11434", LocalEndpointTrustConfig::Loopback),
            ("http://192.168.1.20:11434", LocalEndpointTrustConfig::Lan),
            ("http://10.0.0.5:11434", LocalEndpointTrustConfig::Lan),
            (
                "https://models.example.com:11434",
                LocalEndpointTrustConfig::Remote,
            ),
            ("http://8.8.8.8:11434", LocalEndpointTrustConfig::Remote),
        ] {
            assert_eq!(classify_local_endpoint_trust(url), expected, "{url}");
        }
    }

    fn local_worker(
        provider: &str,
        runtime: &str,
        local: Option<LocalWorkerConfig>,
        auth_profile: Option<&str>,
    ) -> RegistryWorkerConfig {
        RegistryWorkerConfig {
            adapter: None,
            provider: provider.into(),
            model: "llama3".into(),
            runtime: Some(runtime.into()),
            model_artifact: None,
            auth_profile: auth_profile.map(str::to_owned),
            capability_profile: None,
            runtime_config: None,
            local,
            executable: None,
            capabilities: vec![WorkerCapabilityConfig::Implementation],
            fresh_process_isolation: true,
            context_tokens: 0,
            estimated_cost_microusd: None,
            available: true,
            effort: None,
            permission_mode: None,
            extra_args: vec![],
        }
    }

    fn registry(worker: RegistryWorkerConfig) -> WorkerRegistryConfig {
        WorkerRegistryConfig {
            workers: BTreeMap::from([("w".to_owned(), worker)]),
            capability_profiles: BTreeMap::new(),
            routing: WorkerRoutingConfig::default(),
        }
    }

    fn loopback_local(kind: LocalRuntimeKind) -> LocalWorkerConfig {
        LocalWorkerConfig {
            runtime_kind: kind,
            endpoint: LocalEndpointConfig {
                base_url: "http://127.0.0.1:11434".into(),
                tls: false,
            },
            resources: LocalResourceProfileConfig::default(),
        }
    }

    #[test]
    fn valid_loopback_local_worker_passes() {
        let worker = local_worker(
            LOCAL_PROVIDER,
            "ollama",
            Some(loopback_local(LocalRuntimeKind::Ollama)),
            None,
        );
        registry(worker)
            .validate(&std::collections::BTreeSet::new())
            .unwrap();
    }

    #[test]
    fn provider_local_without_a_local_profile_is_an_ordinary_free_string() {
        // `provider = "local"` is an unremarkable free string other worker
        // kinds already use for unrelated reasons (e.g. a Codex-harness
        // worker labeled "local" for a cheap on-box model); declaring the
        // PRD-063 `local` profile is opt-in, never implied by the string.
        let worker = local_worker(LOCAL_PROVIDER, "ollama", None, None);
        registry(worker)
            .validate(&std::collections::BTreeSet::new())
            .unwrap();
    }

    #[test]
    fn local_profile_requires_local_provider() {
        let worker = local_worker(
            "not-local",
            "ollama",
            Some(loopback_local(LocalRuntimeKind::Ollama)),
            None,
        );
        let error = registry(worker)
            .validate(&std::collections::BTreeSet::new())
            .unwrap_err();
        assert!(error.contains("requires provider"), "{error}");
    }

    #[test]
    fn local_runtime_kind_must_match_declared_runtime() {
        let worker = local_worker(
            LOCAL_PROVIDER,
            "unsloth",
            Some(loopback_local(LocalRuntimeKind::Ollama)),
            None,
        );
        let error = registry(worker)
            .validate(&std::collections::BTreeSet::new())
            .unwrap_err();
        assert!(error.contains("runtime_kind must match runtime"), "{error}");
    }

    #[test]
    fn non_loopback_endpoint_requires_auth_profile() {
        let mut local = loopback_local(LocalRuntimeKind::Ollama);
        local.endpoint.base_url = "https://gpu-box.lan:11434".into();
        let worker = local_worker(LOCAL_PROVIDER, "ollama", Some(local), None);
        let error = registry(worker)
            .validate(&std::collections::BTreeSet::new())
            .unwrap_err();
        assert!(error.contains("require auth_profile"), "{error}");

        let mut local = loopback_local(LocalRuntimeKind::Ollama);
        local.endpoint.base_url = "https://gpu-box.lan:11434".into();
        local.endpoint.tls = true;
        let worker = local_worker(LOCAL_PROVIDER, "ollama", Some(local), Some("gpu-box"));
        registry(worker)
            .validate(&std::collections::BTreeSet::new())
            .unwrap();
    }

    #[test]
    fn non_loopback_endpoint_requires_tls_true_and_https_scheme() {
        // auth_profile present but plaintext http:// base_url: refused, even
        // though `tls` defaults to false and would otherwise pass unnoticed.
        let mut local = loopback_local(LocalRuntimeKind::Ollama);
        local.endpoint.base_url = "http://gpu-box.lan:11434".into();
        let worker = local_worker(LOCAL_PROVIDER, "ollama", Some(local), Some("gpu-box"));
        let error = registry(worker)
            .validate(&std::collections::BTreeSet::new())
            .unwrap_err();
        assert!(error.contains("tls = true and an https"), "{error}");

        // auth_profile and https:// base_url, but the declared `tls` flag
        // was left false: the flag is load-bearing, not decorative.
        let mut local = loopback_local(LocalRuntimeKind::Ollama);
        local.endpoint.base_url = "https://gpu-box.lan:11434".into();
        local.endpoint.tls = false;
        let worker = local_worker(LOCAL_PROVIDER, "ollama", Some(local), Some("gpu-box"));
        let error = registry(worker)
            .validate(&std::collections::BTreeSet::new())
            .unwrap_err();
        assert!(error.contains("tls = true and an https"), "{error}");
    }

    #[test]
    fn declared_tls_true_must_match_an_https_scheme() {
        let mut local = loopback_local(LocalRuntimeKind::Ollama);
        local.endpoint.tls = true;
        let worker = local_worker(LOCAL_PROVIDER, "ollama", Some(local), None);
        let error = registry(worker)
            .validate(&std::collections::BTreeSet::new())
            .unwrap_err();
        assert!(error.contains("requires an https:// base_url"), "{error}");
    }

    #[test]
    fn local_and_runtime_config_are_mutually_exclusive() {
        let mut worker = local_worker(
            LOCAL_PROVIDER,
            "ollama",
            Some(loopback_local(LocalRuntimeKind::Ollama)),
            None,
        );
        worker.runtime_config = Some(OllamaRuntimeConfig::default());
        let error = registry(worker)
            .validate(&std::collections::BTreeSet::new())
            .unwrap_err();
        assert!(error.contains("mutually exclusive"), "{error}");
    }

    #[test]
    fn artifact_digest_support_is_closed_per_runtime() {
        assert!(LocalRuntimeKind::Ollama.supports_artifact_digest());
        assert!(!LocalRuntimeKind::Unsloth.supports_artifact_digest());
    }
}
