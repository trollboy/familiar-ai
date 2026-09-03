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
