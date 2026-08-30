use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

use crate::FamiliarError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Directory of operator-approved, one-repository policy fragments. A
    /// relative path is resolved beside the main configuration file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repositories_dir: Option<PathBuf>,
    #[serde(default)]
    pub repositories: BTreeMap<String, RepositoryConfig>,
    /// Machine-global inference endpoints. Authentication values never cross
    /// this boundary; `auth` only describes an operator-managed prerequisite.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub watcher: WatcherConfig,
    #[serde(default)]
    pub tray: TrayConfig,
    #[serde(default)]
    pub summary: SummaryConfig,
    #[serde(default)]
    pub rollup: RollupConfig,
    #[serde(default)]
    pub packer: PackerConfig,
    #[serde(default)]
    pub dashboard: DashboardConfig,
    #[serde(default)]
    pub inference: InferenceConfig,
    #[serde(default)]
    pub execution_history: ExecutionHistoryConfig,
    #[serde(default)]
    pub execution_context: ExecutionContextConfig,
    #[serde(default)]
    pub review: ReviewConfig,
    /// Absent means exactly today's behavior: Codex for both roles and no
    /// review-identity consistency checking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<AgentsConfig>,
    /// Agent and deterministic size ceilings used only by `familiar-ai plan`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner: Option<PlannerConfig>,
    /// Adapter-neutral capability registry. When absent, legacy `[agents]`
    /// entries are translated to the historical two-worker registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_registry: Option<WorkerRegistryConfig>,
    #[serde(default)]
    pub driver: DriverConfig,
    #[serde(default)]
    pub worker: WorkerConfig,
    #[serde(default)]
    pub preflight: PreflightConfig,
    #[serde(default)]
    pub delivery: DeliveryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: EndpointProviderKind,
    pub host: String,
    pub auth: AuthDescriptor,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    /// Deploy-target-only capability discovery. Values are diagnostics, not
    /// credentials, and are replaced on every explicit probe.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Deploy-target-only, deliberately finite remote recipe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<DeployRecipeConfig>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EndpointProviderKind {
    Inference,
    DeployTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeployRecipeConfig {
    pub sync_argv: Vec<String>,
    pub restart_argv: Vec<String>,
    pub smoke_argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String", into = "String")]
pub enum AuthDescriptor {
    None,
    CliLogin(String),
    Env(String),
    SshAgent,
}

impl TryFrom<String> for AuthDescriptor {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let value = value.trim();
        if value == "none" {
            Ok(Self::None)
        } else if value == "ssh-agent" {
            Ok(Self::SshAgent)
        } else if let Some(command) = value.strip_prefix("cli-login: ") {
            validate_identifier(command, "auth descriptor")?;
            Ok(Self::CliLogin(command.to_owned()))
        } else if let Some(name) = value.strip_prefix("env: ") {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                Err(format!("invalid auth descriptor '{value}'"))
            } else {
                Ok(Self::Env(name.to_owned()))
            }
        } else {
            Err(format!("invalid auth descriptor '{value}'"))
        }
    }
}

impl From<AuthDescriptor> for String {
    fn from(value: AuthDescriptor) -> Self {
        match value {
            AuthDescriptor::None => "none".into(),
            AuthDescriptor::CliLogin(command) => format!("cli-login: {command}"),
            AuthDescriptor::Env(name) => format!("env: {name}"),
            AuthDescriptor::SshAgent => "ssh-agent".into(),
        }
    }
}

fn validate_identifier(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Err(format!("invalid {field} '{value}'"))
    } else {
        Ok(())
    }
}

impl ProviderConfig {
    pub fn validate(&self, name: &str) -> Result<(), String> {
        validate_identifier(name, "provider name")?;
        match self.kind {
            EndpointProviderKind::Inference => validate_host(&self.host)?,
            EndpointProviderKind::DeployTarget => validate_ssh_host(&self.host)?,
        }
        for model in &self.models {
            validate_model_identifier(model)?;
        }
        match self.kind {
            EndpointProviderKind::Inference => {
                if self.recipe.is_some() || !self.capabilities.is_empty() {
                    return Err("inference provider has deploy-target extension fields".into());
                }
            }
            EndpointProviderKind::DeployTarget => {
                if self.auth != AuthDescriptor::SshAgent {
                    return Err("deploy-target auth must be ssh-agent".into());
                }
                if !self.models.is_empty() {
                    return Err("deploy-target provider cannot declare models".into());
                }
                let recipe = self
                    .recipe
                    .as_ref()
                    .ok_or("deploy-target recipe is missing")?;
                if recipe.sync_argv.is_empty()
                    || recipe.restart_argv.is_empty()
                    || recipe.smoke_argv.is_empty()
                    || recipe
                        .sync_argv
                        .iter()
                        .chain(&recipe.restart_argv)
                        .chain(&recipe.smoke_argv)
                        .any(|v| v.is_empty())
                {
                    return Err("deploy-target recipe commands must be non-empty".into());
                }
            }
        }
        if let Some(value) = &self.verified_at {
            chrono::DateTime::parse_from_rfc3339(value)
                .map_err(|_| format!("invalid verified_at '{value}'"))?;
        }
        Ok(())
    }
}

fn validate_model_identifier(value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        Err(format!("invalid model '{value}'"))
    } else {
        Ok(())
    }
}

pub fn validate_host(value: &str) -> Result<(), String> {
    let Some((host, port)) = value.rsplit_once(':') else {
        return Err(format!("malformed host '{value}'"));
    };
    if host.is_empty()
        || host.contains(['/', '@', ' '])
        || port.parse::<u16>().ok().filter(|port| *port > 0).is_none()
    {
        return Err(format!("malformed host '{value}'"));
    }
    Ok(())
}

pub fn validate_ssh_host(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.starts_with('-')
        || value.contains(['/', '@', ' ', '\t', '\n'])
        || value.chars().any(char::is_control)
    {
        Err(format!("malformed ssh host '{value}'"))
    } else {
        Ok(())
    }
}

/// Portable user-supervisor settings. The worker deliberately inherits only
/// PATH; credentials required by preflight must be supplied by the supervisor
/// environment and are checked before a PRD is claimed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfig {
    #[serde(default = "default_worker_label")]
    pub label: String,
    #[serde(default = "default_worker_restart_throttle_secs")]
    pub restart_throttle_secs: u64,
    #[serde(default = "default_worker_max_prds")]
    pub max_prds_per_run: u64,
}

fn default_worker_label() -> String {
    "ai.familiar.worker".into()
}
fn default_worker_restart_throttle_secs() -> u64 {
    10
}
fn default_worker_max_prds() -> u64 {
    1
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            label: default_worker_label(),
            restart_throttle_secs: default_worker_restart_throttle_secs(),
            max_prds_per_run: default_worker_max_prds(),
        }
    }
}

impl WorkerConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.label.is_empty()
            || !self
                .label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
        {
            return Err("worker.label must contain only letters, digits, '.', '-', or '_'".into());
        }
        if self.restart_throttle_secs == 0 {
            return Err("worker.restart_throttle_secs must be positive".into());
        }
        if self.max_prds_per_run == 0 {
            return Err("worker.max_prds_per_run must be positive and finite".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeliveryConfig {
    #[serde(default = "default_delivery_mode")]
    pub mode: DeliveryMode,
    /// Legacy compatibility input; it never grants automatic authority.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub max_deliveries_per_session: u64,
    #[serde(default = "default_delivery_command_timeout_ms")]
    pub command_timeout_ms: u64,
    #[serde(default = "default_delivery_remote")]
    pub remote: String,
    #[serde(default = "default_delivery_base")]
    pub base: String,
    /// Provider adapter executable and arguments; no repository or account
    /// identity is embedded.
    #[serde(default)]
    pub provider_argv: Vec<String>,
    /// Legacy compatibility input; repository mode remains authoritative.
    #[serde(default)]
    pub auto_merge: bool,
    #[serde(default)]
    pub staging_environment: String,
    #[serde(default)]
    pub deploy_argv: Vec<String>,
    #[serde(default)]
    pub smoke_argv: Vec<String>,
    #[serde(default)]
    pub rollback_argv: Vec<String>,
    #[serde(default)]
    pub comment_blockers: bool,
    #[serde(default)]
    pub required_checks: Vec<String>,
    #[serde(default)]
    pub migration_gate_argv: Vec<String>,
    #[serde(default)]
    pub credential_references: Vec<String>,
    #[serde(default)]
    pub poc_warrant: Option<PocSelfApprovalWarrant>,
    #[serde(default)]
    pub review_gate: Option<ReviewGateConfig>,
    /// Repository-local environment role to machine-global deploy target.
    #[serde(default)]
    pub targets: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Disabled,
    ReviewedPrManual,
    PocSelfApproval,
    ReviewGatedAutomatic,
}

fn default_delivery_mode() -> DeliveryMode {
    DeliveryMode::ReviewedPrManual
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            mode: DeliveryMode::Disabled,
            enabled: false,
            max_deliveries_per_session: 0,
            command_timeout_ms: default_delivery_command_timeout_ms(),
            remote: default_delivery_remote(),
            base: default_delivery_base(),
            provider_argv: Vec::new(),
            auto_merge: false,
            staging_environment: String::new(),
            deploy_argv: Vec::new(),
            smoke_argv: Vec::new(),
            rollback_argv: Vec::new(),
            comment_blockers: false,
            required_checks: Vec::new(),
            migration_gate_argv: Vec::new(),
            credential_references: Vec::new(),
            poc_warrant: None,
            review_gate: None,
            targets: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PocSelfApprovalWarrant {
    pub actor: String,
    pub max_prds: u64,
    pub expires_at: String,
    #[serde(default = "low_assurance_label")]
    pub assurance_label: String,
}

fn low_assurance_label() -> String {
    "LOW_ASSURANCE_POC_SELF_APPROVAL".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewGateConfig {
    pub implementer: String,
    pub reviewer: String,
    pub approver: String,
}

fn default_delivery_remote() -> String {
    String::new()
}

fn default_delivery_base() -> String {
    String::new()
}

fn default_delivery_command_timeout_ms() -> u64 {
    1_800_000
}

impl DeliveryConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.mode == DeliveryMode::Disabled {
            return Ok(());
        }
        for (role, target) in &self.targets {
            validate_identifier(role, "delivery role")?;
            validate_identifier(target, "deploy target")?;
        }
        if self.max_deliveries_per_session == 0 {
            return Err("delivery requires a finite max_deliveries_per_session".into());
        }
        if self.command_timeout_ms == 0 {
            return Err("delivery requires a finite command_timeout_ms".into());
        }
        if self.remote.trim().is_empty() || self.base.trim().is_empty() {
            return Err("delivery remote and base must be non-empty".into());
        }
        if self.provider_argv.is_empty() {
            return Err("delivery requires a configured provider_argv adapter".into());
        }
        if self.automatically_authorized() {
            if self.staging_environment.trim().is_empty() {
                return Err("automatic delivery staging_environment must be configured".into());
            }
            if self.deploy_argv.is_empty()
                || self.smoke_argv.is_empty()
                || self.rollback_argv.is_empty()
            {
                return Err("automatic delivery requires deploy, smoke, and rollback argv".into());
            }
        }
        if self
            .credential_references
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err("delivery credential references must be non-empty names".into());
        }
        match self.mode {
            DeliveryMode::PocSelfApproval => {
                let warrant = self
                    .poc_warrant
                    .as_ref()
                    .ok_or_else(|| "PoC self-approval requires an explicit warrant".to_owned())?;
                if !warrant.actor.starts_with("human:")
                    || warrant.max_prds == 0
                    || warrant.expires_at.trim().is_empty()
                {
                    return Err(
                        "PoC self-approval warrant requires a human: actor, finite max_prds, and expires_at"
                            .into(),
                    );
                }
                if warrant.assurance_label != low_assurance_label() {
                    return Err("PoC self-approval must use the visible LOW_ASSURANCE_POC_SELF_APPROVAL label".into());
                }
                if chrono::DateTime::parse_from_rfc3339(&warrant.expires_at).is_err() {
                    return Err("PoC self-approval warrant expires_at must be RFC3339".into());
                }
                if self.max_deliveries_per_session > warrant.max_prds {
                    return Err("PoC delivery session cannot exceed the warrant max_prds".into());
                }
                if self.staging_environment.eq_ignore_ascii_case("production")
                    || self.staging_environment.eq_ignore_ascii_case("prod")
                {
                    return Err("PoC self-approval prohibits production delivery".into());
                }
            }
            DeliveryMode::ReviewGatedAutomatic => {
                let gate = self.review_gate.as_ref().ok_or_else(|| "review-gated automatic delivery requires implementer, reviewer, and approver identities".to_owned())?;
                if gate.implementer.trim().is_empty()
                    || gate.reviewer.trim().is_empty()
                    || gate.approver.trim().is_empty()
                    || gate.implementer == gate.reviewer
                    || gate.implementer == gate.approver
                    || gate.reviewer == gate.approver
                {
                    return Err("review-gated delivery requires three distinct non-empty implementer, reviewer, and approver identities".into());
                }
            }
            DeliveryMode::Disabled | DeliveryMode::ReviewedPrManual => {}
        }
        Ok(())
    }

    pub fn automatically_authorized(&self) -> bool {
        matches!(
            self.mode,
            DeliveryMode::PocSelfApproval | DeliveryMode::ReviewGatedAutomatic
        )
    }
}

/// Deterministic prerequisites checked before any PRD is claimed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreflightConfig {
    #[serde(default)]
    pub commands: Vec<PreflightCommandConfig>,
    /// Environment variable names whose values must exist and be non-empty.
    /// Values are never persisted or printed.
    #[serde(default)]
    pub required_environment: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreflightCommandConfig {
    pub check_id: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub working_directory: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    #[serde(default = "default_profile_name")]
    pub profile: String,
    #[serde(default = "default_active_dir")]
    pub active_dir: String,
    #[serde(default = "default_archived_dir")]
    pub archived_dir: String,
    /// `incremental` accepts legacy documents with exact migration diagnostics;
    /// `strict` requires the structured front-matter contract.
    #[serde(default = "default_prd_metadata_policy")]
    pub prd_metadata_policy: String,
    #[serde(default)]
    pub reference_roots: Vec<ReferenceRootConfig>,
    /// Closed vocabulary of permitted `risk_classes` values. Structured PRD
    /// parsing and `metadata-check` reject any declared risk class outside
    /// it; an unconfigured or empty vocabulary rejects every structured PRD
    /// that declares risk classes.
    #[serde(default)]
    pub risk_vocabulary: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_context: Option<ExecutionContextConfig>,
    /// Repository-owned delivery authority. Absence is fail-closed at the
    /// publication boundary; the global legacy delivery section grants none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRootConfig {
    pub prefix: String,
    pub kind: ReferenceKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReferenceKind {
    Prd,
    Adr,
    Contract,
    Supporting,
}

fn default_profile_name() -> String {
    "canonical".into()
}
fn default_active_dir() -> String {
    "docs/prds".into()
}
fn default_archived_dir() -> String {
    "docs/prds/done".into()
}
fn default_prd_metadata_policy() -> String {
    "incremental".into()
}

impl Default for RepositoryConfig {
    fn default() -> Self {
        Self {
            profile: default_profile_name(),
            active_dir: default_active_dir(),
            archived_dir: default_archived_dir(),
            prd_metadata_policy: default_prd_metadata_policy(),
            reference_roots: Vec::new(),
            risk_vocabulary: Vec::new(),
            review: None,
            execution_context: None,
            delivery: None,
        }
    }
}

impl RepositoryConfig {
    pub fn delivery_policy(&self) -> Result<&DeliveryConfig, String> {
        self.delivery.as_ref().ok_or_else(|| {
            "repository delivery policy is missing; merge and deploy are not authorized".into()
        })
    }
    pub fn layout(&self) -> crate::BacklogLayout {
        crate::BacklogLayout {
            profile: crate::BacklogProfile::parse(&self.profile).expect("validated profile"),
            active_dir: crate::RepositoryPath::new(self.active_dir.clone())
                .expect("validated active_dir"),
            archived_dir: crate::RepositoryPath::new(self.archived_dir.clone())
                .expect("validated archived_dir"),
            metadata_policy: crate::PrdMetadataPolicy::parse(&self.prd_metadata_policy)
                .expect("validated prd_metadata_policy"),
            risk_vocabulary: self.risk_vocabulary.clone(),
        }
    }
    pub fn resolved_reference_roots(&self) -> Vec<ReferenceRootConfig> {
        if self.reference_roots.is_empty() {
            default_reference_roots()
        } else {
            self.reference_roots.clone()
        }
    }
}

fn default_reference_roots() -> Vec<ReferenceRootConfig> {
    [
        ("docs/adr/", ReferenceKind::Adr),
        ("docs/contracts/", ReferenceKind::Contract),
        ("docs/supporting/", ReferenceKind::Supporting),
    ]
    .into_iter()
    .map(|(prefix, kind)| ReferenceRootConfig {
        prefix: prefix.into(),
        kind,
    })
    .collect()
}

/// The unattended driver's budget warrant. Every ceiling is optional
/// individually (0 means unlimited), but `drive` refuses to start unless at
/// least one is finite: an unbounded unattended loop is not a warrant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriverConfig {
    #[serde(default)]
    pub max_prds_per_session: u64,
    #[serde(default)]
    pub max_session_cost_microusd: u64,
    #[serde(default)]
    pub max_session_tokens: u64,
    #[serde(default)]
    pub max_session_duration_ms: u64,
    #[serde(default = "default_driver_concurrency")]
    pub max_concurrency: usize,
    #[serde(default)]
    pub isolated_worktrees: bool,
    /// Maximum number of independent dependency components that may execute.
    /// One preserves the original serial driver and primary worktree exactly.
    #[serde(default = "default_driver_concurrency")]
    pub max_parallel_components: usize,
    /// Optional worktree parent. Empty uses the driver-owned state directory.
    #[serde(default)]
    pub worktree_root: String,
    /// Removed legacy implementation routes. Retained only so stale
    /// configuration can fail with an actionable replacement path.
    #[serde(default)]
    pub model_routes: Vec<DriverModelRouteConfig>,
    /// Finite implementation-stage token ceiling. Zero disables this ceiling.
    #[serde(default)]
    pub max_implementation_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DriverModelRouteConfig {
    pub max_expected_files: usize,
    pub model: String,
}

fn default_driver_concurrency() -> usize {
    1
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            max_prds_per_session: 0,
            max_session_cost_microusd: 0,
            max_session_tokens: 0,
            max_session_duration_ms: 0,
            max_concurrency: default_driver_concurrency(),
            isolated_worktrees: false,
            max_parallel_components: default_driver_concurrency(),
            worktree_root: String::new(),
            model_routes: Vec::new(),
            max_implementation_tokens: 0,
        }
    }
}

impl DriverConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_prds_per_session == 0
            && self.max_session_cost_microusd == 0
            && self.max_session_tokens == 0
            && self.max_session_duration_ms == 0
        {
            return Err(
                "unattended drive requires at least one finite ceiling in [driver]: \
                 max_prds_per_session, max_session_cost_microusd, max_session_tokens, or max_session_duration_ms"
                    .into(),
            );
        }
        if self.max_concurrency == 0 {
            return Err("driver.max_concurrency must be positive".into());
        }
        if self.max_parallel_components == 0 {
            return Err("driver.max_parallel_components must be positive".into());
        }
        if self.max_concurrency > 1 && !self.isolated_worktrees {
            return Err(
                "driver.isolated_worktrees must be true when max_concurrency is greater than one"
                    .into(),
            );
        }
        if !self.model_routes.is_empty() {
            return Err(
                "driver.model_routes has been removed; configure worker_registry.routing.rules instead"
                    .into(),
            );
        }
        Ok(())
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegistryWorkerConfig {
    pub adapter: AgentAdapterKind,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub executable: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<WorkerCapabilityConfig>,
    #[serde(default)]
    pub fresh_process_isolation: bool,
    #[serde(default)]
    pub context_tokens: u64,
    #[serde(default)]
    pub estimated_cost_microusd: u64,
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
    pub fn as_agent_entry(&self) -> AgentEntryConfig {
        let model = match self.adapter {
            AgentAdapterKind::Ollama if !self.model.starts_with("ollama/") => {
                Some(format!("ollama/{}", self.model))
            }
            _ => Some(self.model.clone()),
        };
        AgentEntryConfig {
            adapter: self.adapter,
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
    pub routing: WorkerRoutingConfig,
}

impl WorkerRegistryConfig {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_max_review_attempts")]
    pub max_review_attempts: u32,
    #[serde(default = "default_max_remediation_attempts")]
    pub max_remediation_attempts: u32,
    #[serde(default)]
    pub max_total_tokens: u64,
    #[serde(default)]
    pub max_total_cost_microusd: u64,
    #[serde(default)]
    pub max_total_duration_ms: u64,
    #[serde(default)]
    pub allow_isolated_same_model_fallback: bool,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub prohibited_changes: Vec<ProhibitedChangeConfig>,
    #[serde(default)]
    pub scope: ReviewScopeConfig,
    #[serde(default)]
    pub verification: Vec<ReviewVerificationConfig>,
    #[serde(default)]
    pub implementation_agent: ReviewAgentConfig,
    #[serde(default)]
    pub reviewer_agent: ReviewAgentConfig,
    /// Optional cost-tier policy. Absence preserves full independent review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_policy: Option<ReviewTierPolicyConfig>,
    #[serde(default = "default_review_package_bytes")]
    pub max_package_bytes: u64,
    #[serde(default = "default_review_package_tokens")]
    pub max_package_tokens: u64,
    #[serde(default = "default_review_evidence_bytes")]
    pub max_evidence_bytes: u64,
    #[serde(default = "default_verification_log_bytes")]
    pub max_verification_log_bytes: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewAgentConfig {
    pub adapter_id: String,
    pub agent_id: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewTierPolicyConfig {
    /// Repositories at assurance levels requiring independent review cannot
    /// configure a checks-only rule.
    #[serde(default)]
    pub independent_review_required: bool,
    #[serde(default)]
    pub standard_reviewer_agent: ReviewAgentConfig,
    /// Declared PRD risk classes that always require full review.
    #[serde(default)]
    pub full_review_risk_classes: Vec<String>,
    #[serde(default)]
    pub rules: Vec<ReviewTierRuleConfig>,
}

impl ReviewTierPolicyConfig {
    fn validate_risk_vocabulary(&self, risk_vocabulary: &BTreeSet<&str>) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        for class in &self.full_review_risk_classes {
            if class.trim().is_empty() || class.trim() != class {
                return Err(
                    "tier_policy.full_review_risk_classes entries must be non-empty and trimmed"
                        .into(),
                );
            }
            if !seen.insert(class.as_str()) {
                return Err(format!(
                    "tier_policy.full_review_risk_classes contains duplicate class '{class}'"
                ));
            }
            if !risk_vocabulary.contains(class.as_str()) {
                return Err(format!(
                    "tier_policy.full_review_risk_classes names risk class '{class}' outside the configured repository risk vocabulary"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewTierConfig {
    ChecksOnly,
    Standard,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewTierRuleConfig {
    pub id: String,
    pub tier: ReviewTierConfig,
    #[serde(default)]
    pub path_prefixes: Vec<String>,
    #[serde(default)]
    pub max_changed_files: Option<u64>,
    #[serde(default)]
    pub max_changed_bytes: Option<u64>,
    #[serde(default)]
    pub change_kinds: Vec<String>,
    #[serde(default)]
    pub scope_classes: Vec<ScopeFileClassName>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopeDeclarationModeConfig {
    ExpectedOrConfigured,
    ExpectedRequired,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopeClassPolicyConfig {
    Deny,
    HumanReview,
    AllowWhenExpected,
    AllowWhenConfigured,
    Allow,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ScopeFileClassName {
    DependencyManifest,
    DependencyLockfile,
    Migration,
    Configuration,
    Test,
    GeneratedArtifact,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeFileClassConfig {
    #[serde(default = "human_review_policy")]
    pub dependency_manifest: ScopeClassPolicyConfig,
    #[serde(default = "human_review_policy")]
    pub dependency_lockfile: ScopeClassPolicyConfig,
    #[serde(default = "human_review_policy")]
    pub migration: ScopeClassPolicyConfig,
    #[serde(default = "human_review_policy")]
    pub configuration: ScopeClassPolicyConfig,
    #[serde(default = "allow_when_expected_policy")]
    pub test: ScopeClassPolicyConfig,
    #[serde(default = "human_review_policy")]
    pub generated_artifact: ScopeClassPolicyConfig,
}

const fn human_review_policy() -> ScopeClassPolicyConfig {
    ScopeClassPolicyConfig::HumanReview
}
const fn allow_when_expected_policy() -> ScopeClassPolicyConfig {
    ScopeClassPolicyConfig::AllowWhenExpected
}

impl Default for ScopeFileClassConfig {
    fn default() -> Self {
        Self {
            dependency_manifest: human_review_policy(),
            dependency_lockfile: human_review_policy(),
            migration: human_review_policy(),
            configuration: human_review_policy(),
            test: allow_when_expected_policy(),
            generated_artifact: human_review_policy(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeClassificationConfig {
    pub id: String,
    pub class: ScopeFileClassName,
    pub path: String,
    #[serde(default)]
    pub precedence: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewScopeConfig {
    #[serde(default)]
    pub allow_prd_expected_file_expansion: bool,
    #[serde(default = "default_declaration_mode")]
    pub declaration_mode: ScopeDeclarationModeConfig,
    #[serde(default)]
    pub file_classes: ScopeFileClassConfig,
    #[serde(default)]
    pub classification: Vec<ScopeClassificationConfig>,
}

const fn default_declaration_mode() -> ScopeDeclarationModeConfig {
    ScopeDeclarationModeConfig::ExpectedOrConfigured
}

impl Default for ReviewScopeConfig {
    fn default() -> Self {
        Self {
            allow_prd_expected_file_expansion: false,
            declaration_mode: default_declaration_mode(),
            file_classes: ScopeFileClassConfig::default(),
            classification: Vec::new(),
        }
    }
}

pub const SUPPORTED_CHANGE_KINDS: [&str; 7] = [
    "added",
    "modified",
    "deleted",
    "renamed",
    "copied",
    "type_changed",
    "unmerged",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ProhibitedChangeConfig {
    Typed(TypedProhibitedChange),
    Legacy(String),
}

impl From<&str> for ProhibitedChangeConfig {
    fn from(value: &str) -> Self {
        Self::Legacy(value.into())
    }
}
impl From<String> for ProhibitedChangeConfig {
    fn from(value: String) -> Self {
        Self::Legacy(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TypedProhibitedChange {
    pub id: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub class: Option<ScopeFileClassName>,
    #[serde(default)]
    pub change_kinds: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// A prohibited-change rule after lossless resolution of the closed legacy grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProhibitedRule {
    pub id: String,
    pub path: Option<String>,
    pub class: Option<ScopeFileClassName>,
    pub change_kinds: Vec<String>,
    pub description: Option<String>,
}

/// Closed scope-path grammar shared with review's Expected Files contract:
/// exact repository-relative file, `dir/`, or `dir/**` (normalized to `dir/`).
pub fn validate_scope_path(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err("empty path expression".into());
    }
    if value.starts_with('/') {
        return Err("absolute paths are not supported".into());
    }
    if value.contains('\\') {
        return Err("backslashes are not supported".into());
    }
    if value.chars().any(char::is_whitespace) {
        return Err("whitespace is not supported in path expressions".into());
    }
    if value.starts_with('~') {
        return Err("home expansion is not supported".into());
    }
    if value.contains('$') {
        return Err("variable expansion is not supported".into());
    }
    if value.contains(':') {
        return Err("URI forms are not supported".into());
    }
    let (body, directory) = if let Some(prefix) = value.strip_suffix("/**") {
        (prefix, true)
    } else if let Some(prefix) = value.strip_suffix('/') {
        (prefix, true)
    } else {
        (value, false)
    };
    if body.is_empty() {
        return Err("empty path expression".into());
    }
    if body.contains(['*', '?', '{', '}', '[', ']']) {
        return Err("glob syntax other than a terminal '/**' is not supported".into());
    }
    if body
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("empty, '.' or '..' path components are not supported".into());
    }
    Ok(if directory {
        format!("{body}/")
    } else {
        body.to_owned()
    })
}

impl ProhibitedChangeConfig {
    /// Resolve under the closed legacy grammar: a path-shaped string becomes a
    /// typed path rule; the exact phrase `dependency changes` becomes typed
    /// prohibitions of the dependency manifest and lockfile classes; any other
    /// free-form string fails with a migration diagnostic.
    pub fn resolve(&self) -> Result<Vec<ResolvedProhibitedRule>, String> {
        match self {
            Self::Legacy(value) if value == "dependency changes" => Ok(vec![
                ResolvedProhibitedRule {
                    id: "legacy:class:dependency_manifest".into(),
                    path: None,
                    class: Some(ScopeFileClassName::DependencyManifest),
                    change_kinds: Vec::new(),
                    description: Some("legacy 'dependency changes' prohibition".into()),
                },
                ResolvedProhibitedRule {
                    id: "legacy:class:dependency_lockfile".into(),
                    path: None,
                    class: Some(ScopeFileClassName::DependencyLockfile),
                    change_kinds: Vec::new(),
                    description: Some("legacy 'dependency changes' prohibition".into()),
                },
            ]),
            Self::Legacy(value) => {
                // A bare word ("commit", "push", "deployment") is not a path
                // under the closed legacy grammar even though it is a valid
                // single path component; legacy path strings must look like
                // paths so free-form prose always fails closed.
                let path_shaped = value.contains('/') || value.contains('.');
                match validate_scope_path(value) {
                    Ok(normalized) if path_shaped => Ok(vec![ResolvedProhibitedRule {
                        id: format!("legacy:path:{normalized}"),
                        path: Some(normalized),
                        class: None,
                        change_kinds: Vec::new(),
                        description: Some(format!("legacy prohibited path '{value}'")),
                    }]),
                    _ => Err(format!(
                        "prohibited_changes entry '{value}' is not in the closed legacy grammar \
                         (a repository-relative path containing '/' or '.', or the exact phrase \
                         'dependency changes'); migrate it to a typed \
                         [[review.prohibited_changes]] table with id and path or class"
                    )),
                }
            }
            Self::Typed(rule) => {
                if rule.id.trim().is_empty() {
                    return Err("typed prohibited_changes rule requires a non-empty id".into());
                }
                match (&rule.path, &rule.class) {
                    (Some(_), Some(_)) | (None, None) => {
                        return Err(format!(
                            "prohibited_changes rule '{}' must declare exactly one of path or class",
                            rule.id
                        ));
                    }
                    _ => {}
                }
                let path = match &rule.path {
                    Some(path) => Some(validate_scope_path(path).map_err(|error| {
                        format!("prohibited_changes rule '{}': {error}", rule.id)
                    })?),
                    None => None,
                };
                for kind in &rule.change_kinds {
                    if !SUPPORTED_CHANGE_KINDS.contains(&kind.as_str()) {
                        return Err(format!(
                            "prohibited_changes rule '{}' names unsupported change kind '{kind}'",
                            rule.id
                        ));
                    }
                }
                Ok(vec![ResolvedProhibitedRule {
                    id: rule.id.clone(),
                    path,
                    class: rule.class,
                    change_kinds: rule.change_kinds.clone(),
                    description: rule.description.clone(),
                }])
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewVerificationConfig {
    pub check_id: String,
    pub argv: Vec<String>,
    #[serde(default = "default_working_directory")]
    pub working_directory: String,
    #[serde(default = "default_review_action_duration_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub path_prefixes: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}
fn default_working_directory() -> String {
    ".".into()
}
const fn default_review_action_duration_ms() -> u64 {
    300_000
}
const fn default_review_package_bytes() -> u64 {
    1_000_000
}
const fn default_review_package_tokens() -> u64 {
    250_000
}
const fn default_review_evidence_bytes() -> u64 {
    16_000_000
}
const fn default_verification_log_bytes() -> usize {
    256_000
}

const fn default_max_review_attempts() -> u32 {
    3
}
const fn default_max_remediation_attempts() -> u32 {
    2
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_review_attempts: default_max_review_attempts(),
            max_remediation_attempts: default_max_remediation_attempts(),
            max_total_tokens: 0,
            max_total_cost_microusd: 0,
            max_total_duration_ms: 0,
            allow_isolated_same_model_fallback: false,
            allowed_paths: Vec::new(),
            prohibited_changes: Vec::new(),
            scope: ReviewScopeConfig::default(),
            verification: Vec::new(),
            implementation_agent: ReviewAgentConfig::default(),
            reviewer_agent: ReviewAgentConfig::default(),
            tier_policy: None,
            max_package_bytes: default_review_package_bytes(),
            max_package_tokens: default_review_package_tokens(),
            max_evidence_bytes: default_review_evidence_bytes(),
            max_verification_log_bytes: default_verification_log_bytes(),
        }
    }
}

impl ReviewConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.max_review_attempts == 0 || self.max_remediation_attempts == 0 {
            return Err(
                "enabled review requires finite positive review and remediation attempt limits"
                    .into(),
            );
        }
        if self.max_total_tokens == 0
            && self.max_total_cost_microusd == 0
            && self.max_total_duration_ms == 0
        {
            return Err(
                "enabled review requires at least one enforceable aggregate resource ceiling"
                    .into(),
            );
        }
        if self.allowed_paths.is_empty() && !self.scope.allow_prd_expected_file_expansion {
            return Err(
                "enabled review requires allowed paths unless PRD expected-file expansion is enabled"
                    .into(),
            );
        }
        for path in &self.allowed_paths {
            validate_scope_path(path)
                .map_err(|error| format!("review allowed path '{path}': {error}"))?;
        }
        let mut prohibited_ids = std::collections::BTreeSet::new();
        for entry in &self.prohibited_changes {
            for rule in entry.resolve()? {
                if !prohibited_ids.insert(rule.id.clone()) {
                    return Err(format!(
                        "duplicate prohibited_changes rule id '{}'",
                        rule.id
                    ));
                }
            }
        }
        let mut classification_ids = std::collections::BTreeSet::new();
        for rule in &self.scope.classification {
            if rule.id.trim().is_empty() {
                return Err("scope classification rule requires a non-empty id".into());
            }
            if !classification_ids.insert(rule.id.clone()) {
                return Err(format!(
                    "duplicate scope classification rule id '{}'",
                    rule.id
                ));
            }
            validate_scope_path(&rule.path)
                .map_err(|error| format!("scope classification rule '{}': {error}", rule.id))?;
        }
        if self.verification.is_empty()
            || self.verification.iter().any(|check| {
                check.check_id.is_empty()
                    || check.argv.is_empty()
                    || check.timeout_ms == 0
                    || (!check.argv[0].contains('/') && !check.environment.contains_key("PATH"))
            })
        {
            return Err(
                "enabled review requires allowed paths and non-empty bounded verification checks"
                    .into(),
            );
        }
        if self.implementation_agent.adapter_id.is_empty()
            || self.implementation_agent.agent_id.is_empty()
            || self.reviewer_agent.adapter_id.is_empty()
            || self.reviewer_agent.agent_id.is_empty()
        {
            return Err(
                "enabled review requires explicit implementation and reviewer agent identities"
                    .into(),
            );
        }
        if self.max_package_bytes == 0
            || self.max_package_tokens == 0
            || self.max_evidence_bytes == 0
            || self.max_verification_log_bytes == 0
        {
            return Err("enabled review requires positive package and evidence ceilings".into());
        }
        if let Some(policy) = &self.tier_policy {
            let mut ids = std::collections::BTreeSet::new();
            let mut signatures = std::collections::BTreeMap::new();
            let has_standard = policy
                .rules
                .iter()
                .any(|rule| rule.tier == ReviewTierConfig::Standard);
            if has_standard
                && (policy.standard_reviewer_agent.adapter_id.is_empty()
                    || policy.standard_reviewer_agent.agent_id.is_empty()
                    || !policy
                        .standard_reviewer_agent
                        .model
                        .as_deref()
                        .is_some_and(|model| !model.is_empty()))
            {
                return Err("standard review rules require an explicit tier_policy.standard_reviewer_agent identity and model".into());
            }
            if has_standard
                && policy.standard_reviewer_agent.adapter_id != self.reviewer_agent.adapter_id
            {
                return Err(
                    "tier_policy.standard_reviewer_agent must use the configured reviewer adapter"
                        .into(),
                );
            }
            if has_standard && policy.standard_reviewer_agent.model == self.reviewer_agent.model {
                return Err(
                    "tier_policy.standard_reviewer_agent model must differ from the full reviewer model"
                        .into(),
                );
            }
            for rule in &policy.rules {
                if rule.id.trim().is_empty() || !ids.insert(rule.id.clone()) {
                    return Err(format!(
                        "review tier rule id '{}' is empty or duplicated",
                        rule.id
                    ));
                }
                if policy.independent_review_required && rule.tier == ReviewTierConfig::ChecksOnly {
                    return Err(format!("review tier rule '{}' selects checks-only although independent review is required", rule.id));
                }
                if rule.path_prefixes.is_empty()
                    && rule.max_changed_files.is_none()
                    && rule.max_changed_bytes.is_none()
                    && rule.change_kinds.is_empty()
                    && rule.scope_classes.is_empty()
                {
                    return Err(format!(
                        "review tier rule '{}' has no footprint predicates",
                        rule.id
                    ));
                }
                for path in &rule.path_prefixes {
                    validate_scope_path(path)
                        .map_err(|error| format!("review tier rule '{}': {error}", rule.id))?;
                }
                for kind in &rule.change_kinds {
                    if !SUPPORTED_CHANGE_KINDS.contains(&kind.as_str()) {
                        return Err(format!(
                            "review tier rule '{}' names unsupported change kind '{kind}'",
                            rule.id
                        ));
                    }
                }
                let mut paths = rule.path_prefixes.clone();
                let mut kinds = rule.change_kinds.clone();
                let mut classes = rule.scope_classes.clone();
                paths.sort();
                paths.dedup();
                kinds.sort();
                kinds.dedup();
                classes.sort();
                classes.dedup();
                let signature = serde_json::to_string(&(
                    paths,
                    rule.max_changed_files,
                    rule.max_changed_bytes,
                    kinds,
                    classes,
                ))
                .map_err(|error| error.to_string())?;
                if let Some((other, tier)) =
                    signatures.insert(signature, (rule.id.clone(), rule.tier))
                {
                    if tier != rule.tier {
                        return Err(format!(
                            "review tier rules '{other}' and '{}' contradict each other",
                            rule.id
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionContextConfig {
    pub hard_ceiling_tokens: Option<u64>,
    /// Enables adapter-specific prompt cache controls. Unsupported adapters
    /// retain their native behavior and receive no fabricated flags.
    #[serde(default = "default_prompt_cache_enabled")]
    pub prompt_cache_enabled: bool,
}

const fn default_prompt_cache_enabled() -> bool {
    true
}

impl Default for ExecutionContextConfig {
    fn default() -> Self {
        Self {
            hard_ceiling_tokens: None,
            prompt_cache_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationSource {
    Global,
    Repository,
}

impl ConfigurationSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Repository => "repository",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveExecutionConfig {
    pub review: ReviewConfig,
    pub review_source: ConfigurationSource,
    pub execution_context: ExecutionContextConfig,
    pub execution_context_source: ConfigurationSource,
}

/// The canonical Git common-directory identity of a worktree — the same key
/// `FilesystemBacklogDiscovery::resolve` computes — or None when the path is
/// not inside a Git repository (or git is unavailable).
fn git_common_directory(path: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let canonical = Path::new(value.trim()).canonicalize().ok()?;
    Some(canonical.to_str()?.replace('\\', "/"))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionHistoryConfig {
    #[serde(default)]
    pub pricing: BTreeMap<String, ExecutionPrice>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionPrice {
    pub input_microusd_per_million: Option<u64>,
    pub cached_input_microusd_per_million: Option<u64>,
    pub output_microusd_per_million: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    pub pid_file: Option<PathBuf>,
    pub socket_path: Option<PathBuf>,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_secs: u64,
}

fn default_heartbeat_interval() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    pub file: Option<PathBuf>,
    #[serde(default)]
    pub format: LogFormat,
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Pretty,
    Json,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub path: Option<PathBuf>,
}

impl DatabaseConfig {
    pub fn resolve_path(&self, data_dir: &Path) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(|| data_dir.join("familiar.db"))
    }
}

// --- Inference config (replaces old LlmConfig) ---

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InferenceMode {
    #[default]
    Disabled,
    LocalOnly,
    RemoteOnly,
    Hybrid,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    Disabled,
    Builtin,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextInferenceConfig {
    #[serde(default)]
    pub mode: InferenceMode,
    #[serde(default)]
    pub provider: ProviderKind,
    #[serde(default = "default_builtin_text_model")]
    pub builtin_model: String,
    #[serde(default = "default_builtin_url")]
    pub builtin_url: String,
    pub remote_url: Option<String>,
    pub remote_api_key: Option<String>,
    #[serde(default = "default_true")]
    pub fallback_enabled: bool,
    #[serde(default)]
    pub prefer_privacy: bool,
    #[serde(default = "default_true")]
    pub prefer_cost_savings: bool,
    #[serde(default = "default_inference_max_input_chars")]
    pub max_input_chars: usize,
    #[serde(default = "default_inference_max_concurrent")]
    pub max_concurrent_requests: usize,
    #[serde(default = "default_inference_timeout_secs")]
    pub request_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingInferenceConfig {
    #[serde(default)]
    pub provider: ProviderKind,
    #[serde(default = "default_builtin_embedding_model")]
    pub builtin_model: String,
    #[serde(default = "default_builtin_url")]
    pub builtin_url: String,
    pub remote_url: Option<String>,
    pub remote_api_key: Option<String>,
    #[serde(default = "default_true")]
    pub fallback_enabled: bool,
    #[serde(default = "default_inference_max_concurrent")]
    pub max_concurrent_requests: usize,
    #[serde(default = "default_inference_timeout_secs")]
    pub request_timeout_secs: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceConfig {
    #[serde(default)]
    pub text: TextInferenceConfig,
    #[serde(default)]
    pub embedding: EmbeddingInferenceConfig,
}

fn default_builtin_text_model() -> String {
    "qwen2.5:3b".to_string()
}

fn default_builtin_embedding_model() -> String {
    "nomic-embed-text".to_string()
}

fn default_builtin_url() -> String {
    "http://127.0.0.1:11434/v1".to_string()
}

fn default_true() -> bool {
    true
}

fn default_inference_max_input_chars() -> usize {
    16_000
}

fn default_inference_max_concurrent() -> usize {
    4
}

fn default_inference_timeout_secs() -> u64 {
    60
}

impl Default for TextInferenceConfig {
    fn default() -> Self {
        Self {
            mode: InferenceMode::default(),
            provider: ProviderKind::default(),
            builtin_model: default_builtin_text_model(),
            builtin_url: default_builtin_url(),
            remote_url: None,
            remote_api_key: None,
            fallback_enabled: true,
            prefer_privacy: false,
            prefer_cost_savings: true,
            max_input_chars: default_inference_max_input_chars(),
            max_concurrent_requests: default_inference_max_concurrent(),
            request_timeout_secs: default_inference_timeout_secs(),
        }
    }
}

impl Default for EmbeddingInferenceConfig {
    fn default() -> Self {
        Self {
            provider: ProviderKind::default(),
            builtin_model: default_builtin_embedding_model(),
            builtin_url: default_builtin_url(),
            remote_url: None,
            remote_api_key: None,
            fallback_enabled: true,
            max_concurrent_requests: default_inference_max_concurrent(),
            request_timeout_secs: default_inference_timeout_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    #[serde(default = "default_watcher_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_ignore_patterns")]
    pub ignore_patterns: Vec<String>,
    #[serde(default = "default_respect_gitignore")]
    pub respect_gitignore: bool,
}

fn default_watcher_enabled() -> bool {
    true
}

fn default_debounce_ms() -> u64 {
    1000
}

fn default_ignore_patterns() -> Vec<String> {
    vec!["target/**".into(), "node_modules/**".into()]
}

fn default_respect_gitignore() -> bool {
    true
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            enabled: default_watcher_enabled(),
            paths: Vec::new(),
            debounce_ms: default_debounce_ms(),
            ignore_patterns: default_ignore_patterns(),
            respect_gitignore: default_respect_gitignore(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrayConfig {
    #[serde(default = "default_tray_enabled")]
    pub enabled: bool,
    #[serde(default = "default_recent_projects_count")]
    pub recent_projects_count: usize,
}

fn default_tray_enabled() -> bool {
    true
}

fn default_recent_projects_count() -> usize {
    5
}

impl Default for TrayConfig {
    fn default() -> Self {
        Self {
            enabled: default_tray_enabled(),
            recent_projects_count: default_recent_projects_count(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryConfig {
    #[serde(default = "default_summary_enabled")]
    pub enabled: bool,
    #[serde(default = "default_staleness_threshold_secs")]
    pub staleness_threshold_secs: u64,
    #[serde(default = "default_flush_interval_secs")]
    pub flush_interval_secs: u64,
    #[serde(default = "default_max_file_size_bytes")]
    pub max_file_size_bytes: u64,
    #[serde(default = "default_max_pending_files")]
    pub max_pending_files: usize,
    #[serde(default = "default_per_file_quiet_ms")]
    pub per_file_quiet_ms: u64,
}

fn default_summary_enabled() -> bool {
    true
}

fn default_staleness_threshold_secs() -> u64 {
    86_400 // 24h
}

fn default_flush_interval_secs() -> u64 {
    3
}

fn default_max_file_size_bytes() -> u64 {
    1_048_576 // 1 MB
}

fn default_max_pending_files() -> usize {
    10_000
}

fn default_per_file_quiet_ms() -> u64 {
    1_500
}

impl Default for SummaryConfig {
    fn default() -> Self {
        Self {
            enabled: default_summary_enabled(),
            staleness_threshold_secs: default_staleness_threshold_secs(),
            flush_interval_secs: default_flush_interval_secs(),
            max_file_size_bytes: default_max_file_size_bytes(),
            max_pending_files: default_max_pending_files(),
            per_file_quiet_ms: default_per_file_quiet_ms(),
        }
    }
}

// TODO: future PRDs may want per-project summary settings (e.g., custom
// max_file_size or custom ignore patterns) overriding the global SummaryConfig.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupConfig {
    #[serde(default = "default_rollup_enabled")]
    pub enabled: bool,
    #[serde(default = "default_rollup_default_limit")]
    pub default_limit: usize,
    #[serde(default = "default_max_rollup_tokens")]
    pub max_rollup_tokens: usize,
    #[serde(default = "default_max_rollup_chars")]
    pub max_rollup_chars: usize,
}

fn default_rollup_enabled() -> bool {
    true
}

fn default_rollup_default_limit() -> usize {
    20
}

fn default_max_rollup_tokens() -> usize {
    4000
}

fn default_max_rollup_chars() -> usize {
    50_000
}

impl Default for RollupConfig {
    fn default() -> Self {
        Self {
            enabled: default_rollup_enabled(),
            default_limit: default_rollup_default_limit(),
            max_rollup_tokens: default_max_rollup_tokens(),
            max_rollup_chars: default_max_rollup_chars(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BudgetProfile {
    Minimal,
    #[default]
    Balanced,
    Aggressive,
    MaxAccuracy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackerConfig {
    #[serde(default)]
    pub default_profile: BudgetProfile,
    #[serde(default = "default_packer_hard_ceiling")]
    pub hard_ceiling_tokens: usize,
}

fn default_packer_hard_ceiling() -> usize {
    15_000
}

impl Default for PackerConfig {
    fn default() -> Self {
        Self {
            default_profile: BudgetProfile::default(),
            hard_ceiling_tokens: default_packer_hard_ceiling(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_dashboard_bind")]
    pub bind_address: String,
}

fn default_dashboard_bind() -> String {
    "127.0.0.1:9400".to_string()
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: default_dashboard_bind(),
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            pid_file: None,
            socket_path: None,
            heartbeat_interval_secs: default_heartbeat_interval(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: None,
            format: LogFormat::default(),
        }
    }
}

/// The configuration environment prefix.
pub const ENV_PREFIX: &str = "FAMILIAR_AI_";
/// The removed pre-rename prefix, named only so stale configuration fails
/// closed instead of being silently ignored.
/// identity-gate exception: the legacy prefix is intentional here.
pub const LEGACY_ENV_PREFIX: &str = "FAMILIAR_"; // identity-gate: allow

/// Legacy-prefixed variable names in `keys`, sorted. Non-empty means
/// configuration loading must fail closed.
pub fn stale_legacy_env(keys: impl Iterator<Item = String>) -> Vec<String> {
    let mut stale: Vec<String> = keys
        .filter(|key| key.starts_with(LEGACY_ENV_PREFIX) && !key.starts_with(ENV_PREFIX))
        .collect();
    stale.sort();
    stale
}

fn reject_stale_env() -> crate::Result<()> {
    let stale = stale_legacy_env(std::env::vars_os().filter_map(|(key, _)| key.into_string().ok()));
    if stale.is_empty() {
        Ok(())
    } else {
        Err(FamiliarError::Config(format!(
            "stale legacy environment variables use the removed {LEGACY_ENV_PREFIX} prefix: {}; \
             rename them to the {ENV_PREFIX} prefix",
            stale.join(", ")
        )))
    }
}

impl Config {
    fn validate_preflight(&self) -> crate::Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        for check in &self.preflight.commands {
            if check.check_id.trim().is_empty() || check.argv.is_empty() {
                return Err(FamiliarError::Config(
                    "preflight commands require a non-empty check_id and argv".into(),
                ));
            }
            if !ids.insert(check.check_id.as_str()) {
                return Err(FamiliarError::Config(format!(
                    "duplicate preflight check_id {:?}",
                    check.check_id
                )));
            }
            if check.argv.iter().any(|arg| arg.is_empty()) {
                return Err(FamiliarError::Config(format!(
                    "preflight check {:?} contains an empty argv element",
                    check.check_id
                )));
            }
            if Path::new(&check.working_directory).is_absolute()
                || check.working_directory.split('/').any(|part| part == "..")
            {
                return Err(FamiliarError::Config(format!(
                    "preflight check {:?} working_directory must be repository-relative",
                    check.check_id
                )));
            }
        }
        for name in &self.preflight.required_environment {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|character| character == '_' || character.is_ascii_alphanumeric())
            {
                return Err(FamiliarError::Config(format!(
                    "invalid preflight environment variable name {name:?}"
                )));
            }
        }
        Ok(())
    }

    fn validate_repositories(&self) -> crate::Result<()> {
        let mut resolved = BTreeMap::<PathBuf, String>::new();
        for (worktree, entry) in &self.repositories {
            crate::BacklogProfile::parse(&entry.profile).map_err(FamiliarError::Config)?;
            crate::PrdMetadataPolicy::parse(&entry.prd_metadata_policy)
                .map_err(FamiliarError::Config)?;
            for (label, value) in [
                ("active_dir", &entry.active_dir),
                ("archived_dir", &entry.archived_dir),
            ] {
                if value.contains('\\') || Path::new(value).is_absolute() {
                    return Err(FamiliarError::Config(format!(
                        "repositories.{worktree}.{label} must be repository-relative and traversal-free; offending value '{value}'"
                    )));
                }
                crate::RepositoryPath::new(value.clone()).map_err(|_| FamiliarError::Config(format!("repositories.{worktree}.{label} must be repository-relative and traversal-free; offending value '{value}'")))?;
            }
            if entry.active_dir == entry.archived_dir {
                return Err(FamiliarError::Config(format!(
                    "repositories.{worktree} active_dir and archived_dir must be distinct: '{}'",
                    entry.active_dir
                )));
            }
            for root in &entry.reference_roots {
                crate::RepositoryPath::new(root.prefix.trim_end_matches('/').to_owned()).map_err(
                    |_| {
                        FamiliarError::Config(format!(
                            "repositories.{worktree}.reference_roots contains invalid prefix '{}'",
                            root.prefix
                        ))
                    },
                )?;
                if !root.prefix.ends_with('/') {
                    return Err(FamiliarError::Config(format!(
                        "repositories.{worktree}.reference_roots prefix must end with '/': '{}'",
                        root.prefix
                    )));
                }
            }
            let mut seen_risk_classes = std::collections::BTreeSet::new();
            for class in &entry.risk_vocabulary {
                if class.trim().is_empty() {
                    return Err(FamiliarError::Config(format!(
                        "repositories.{worktree}.risk_vocabulary entries must be non-empty"
                    )));
                }
                if class.trim() != class {
                    return Err(FamiliarError::Config(format!(
                        "repositories.{worktree}.risk_vocabulary class '{class}' must not have leading or trailing whitespace"
                    )));
                }
                if !seen_risk_classes.insert(class) {
                    return Err(FamiliarError::Config(format!(
                        "repositories.{worktree}.risk_vocabulary contains duplicate class '{class}'"
                    )));
                }
            }
            if let Some(review) = &entry.review {
                review.validate().map_err(|error| {
                    FamiliarError::Config(format!("repositories.{worktree}.review: {error}"))
                })?;
                if let Some(policy) = &review.tier_policy {
                    let vocabulary = entry.risk_vocabulary.iter().map(String::as_str).collect();
                    policy
                        .validate_risk_vocabulary(&vocabulary)
                        .map_err(|error| {
                            FamiliarError::Config(format!(
                                "repositories.{worktree}.review: {error}"
                            ))
                        })?;
                }
                if let Some(agents) = &self.agents {
                    agents.validate(review).map_err(|error| {
                        FamiliarError::Config(format!("repositories.{worktree}.review: {error}"))
                    })?;
                }
            }
            if let Some(delivery) = &entry.delivery {
                delivery.validate().map_err(|error| {
                    FamiliarError::Config(format!("repositories.{worktree}.delivery: {error}"))
                })?;
            }
            let absolute = Path::new(worktree);
            if !absolute.is_absolute() {
                return Err(FamiliarError::Config(format!(
                    "repository worktree key must be absolute: '{worktree}'"
                )));
            }
            let canonical = absolute.canonicalize().map_err(|e| {
                FamiliarError::Config(format!(
                    "cannot canonicalize repository worktree '{worktree}': {e}"
                ))
            })?;
            if let Some(first) = resolved.insert(canonical.clone(), worktree.clone()) {
                return Err(FamiliarError::Config(format!(
                    "repository entries '{first}' and '{worktree}' resolve to the same worktree {}",
                    canonical.display()
                )));
            }
        }
        Ok(())
    }

    /// Resolve every configured entry that names this worktree's repository:
    /// exact canonical-path matches AND entries matched through Git
    /// common-directory repository identity, so a linked worktree (an isolated
    /// lease created in a prior process, say) resolves the same policy as the
    /// main worktree it belongs to without any path-specific configuration
    /// entry (PRD-065). Entries with identical configuration deduplicate (a
    /// drive session injects an execution-root clone of the pinned
    /// repository's entry); entries for one repository with DIFFERENT
    /// configuration fail closed with a diagnostic naming them — never a
    /// silent shadow (review F4).
    fn repository_entry_checked(
        &self,
        canonical_worktree: &Path,
    ) -> crate::Result<Option<&RepositoryConfig>> {
        let identity = git_common_directory(canonical_worktree);
        let mut matches: Vec<(&String, &RepositoryConfig)> = Vec::new();
        for (path, entry) in &self.repositories {
            let Ok(canonical) = Path::new(path).canonicalize() else {
                continue;
            };
            let matched = canonical == canonical_worktree
                || match (&identity, git_common_directory(&canonical)) {
                    (Some(queried), Some(configured)) => *queried == configured,
                    _ => false,
                };
            if matched {
                matches.push((path, entry));
            }
        }
        let Some((first_path, first_entry)) = matches.first().copied() else {
            return Ok(None);
        };
        if let Some((conflicting_path, _)) = matches.iter().find(|(_, entry)| *entry != first_entry)
        {
            return Err(FamiliarError::Config(format!(
                "repository entries '{first_path}' and '{conflicting_path}' resolve to the same repository{} with different configuration; keep exactly one",
                identity
                    .as_deref()
                    .map(|value| format!(" identity {value}"))
                    .unwrap_or_default()
            )));
        }
        Ok(Some(first_entry))
    }

    pub fn repository(&self, canonical_worktree: &Path) -> crate::Result<RepositoryConfig> {
        Ok(self
            .repository_entry_checked(canonical_worktree)?
            .cloned()
            .unwrap_or_default())
    }

    pub fn effective_execution(
        &self,
        canonical_worktree: &Path,
    ) -> crate::Result<EffectiveExecutionConfig> {
        let entry = self.repository_entry_checked(canonical_worktree)?;
        Ok(EffectiveExecutionConfig {
            review: entry
                .and_then(|entry| entry.review.clone())
                .unwrap_or_else(|| self.review.clone()),
            review_source: if entry.and_then(|entry| entry.review.as_ref()).is_some() {
                ConfigurationSource::Repository
            } else {
                ConfigurationSource::Global
            },
            execution_context: entry
                .and_then(|entry| entry.execution_context.clone())
                .unwrap_or_else(|| self.execution_context.clone()),
            execution_context_source: if entry
                .and_then(|entry| entry.execution_context.as_ref())
                .is_some()
            {
                ConfigurationSource::Repository
            } else {
                ConfigurationSource::Global
            },
        })
    }

    fn validate_execution(&self) -> crate::Result<()> {
        if !self.driver.model_routes.is_empty() {
            return Err(FamiliarError::Config(
                "driver.model_routes has been removed; configure worker_registry.routing.rules instead"
                    .into(),
            ));
        }
        if self.agents.is_some() && self.worker_registry.is_some() {
            return Err(FamiliarError::Config(
                "[agents] and [worker_registry] are mutually exclusive".into(),
            ));
        }
        self.review.validate().map_err(FamiliarError::Config)?;
        if let Some(policy) = &self.review.tier_policy {
            let risk_vocabulary: BTreeSet<&str> = self
                .repositories
                .values()
                .flat_map(|entry| entry.risk_vocabulary.iter().map(String::as_str))
                .collect();
            policy
                .validate_risk_vocabulary(&risk_vocabulary)
                .map_err(FamiliarError::Config)?;
        }
        if let Some(agents) = &self.agents {
            agents
                .validate(&self.review)
                .map_err(FamiliarError::Config)?;
        }
        if let Some(registry) = &self.worker_registry {
            let risk_vocabulary: std::collections::BTreeSet<&str> = self
                .repositories
                .values()
                .flat_map(|entry| entry.risk_vocabulary.iter().map(String::as_str))
                .collect();
            registry
                .validate(&risk_vocabulary)
                .map_err(FamiliarError::Config)?;
        }
        if let Some(planner) = &self.planner {
            planner.validate().map_err(FamiliarError::Config)?;
        }
        Ok(())
    }
    fn validate_providers(&self) -> crate::Result<()> {
        for (name, provider) in &self.providers {
            provider.validate(name).map_err(FamiliarError::Config)?;
        }
        Ok(())
    }
    pub fn load(config_path: Option<&Path>) -> crate::Result<Self> {
        reject_stale_env()?;
        let mut figment = Figment::from(Serialized::defaults(Config::default()));

        if let Some(path) = config_path {
            if path.exists() {
                figment = figment.merge(Toml::file(path));
            }
        }

        // Resolve fragment location from defaults + the main file only. The
        // fragments themselves cannot redirect discovery, and repository
        // content is never consulted while loading configuration.
        let base: Self = figment
            .clone()
            .extract()
            .map_err(|e| FamiliarError::Config(e.to_string()))?;
        if let Some(main_path) = config_path {
            let configured_dir = base
                .repositories_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("repositories"));
            let fragment_dir = if configured_dir.is_absolute() {
                configured_dir
            } else {
                main_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(configured_dir)
            };
            if fragment_dir.is_dir() {
                let mut fragments = std::fs::read_dir(&fragment_dir)
                    .map_err(FamiliarError::Io)?
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.extension().and_then(|value| value.to_str()) == Some("toml")
                    })
                    .collect::<Vec<_>>();
                fragments.sort();
                let mut owners = base.repositories.keys().cloned().collect::<BTreeSet<_>>();
                for fragment in fragments {
                    let parsed: Config = Figment::from(Serialized::defaults(Config::default()))
                        .merge(Toml::file(&fragment))
                        .extract()
                        .map_err(|e| {
                            FamiliarError::Config(format!("{}: {e}", fragment.display()))
                        })?;
                    for key in parsed.repositories.keys() {
                        if !owners.insert(key.clone()) {
                            return Err(FamiliarError::Config(format!(
                                "repository key {key:?} is defined more than once (including {})",
                                fragment.display()
                            )));
                        }
                    }
                    figment = figment.merge(Toml::file(&fragment));
                }
            }
        }

        figment = figment.merge(Env::prefixed(ENV_PREFIX).split("__"));

        let config: Self = figment
            .extract()
            .map_err(|e| FamiliarError::Config(e.to_string()))?;
        config.validate_repositories()?;
        config.validate_providers()?;
        config.validate_execution()?;
        config.validate_preflight()?;
        config.delivery.validate().map_err(FamiliarError::Config)?;
        config.worker.validate().map_err(FamiliarError::Config)?;
        Ok(config)
    }

    pub fn load_with_overrides(
        config_path: Option<&Path>,
        overrides: figment::Figment,
    ) -> crate::Result<Self> {
        reject_stale_env()?;
        let mut figment = Figment::from(Serialized::defaults(Config::default()));

        if let Some(path) = config_path {
            if path.exists() {
                figment = figment.merge(Toml::file(path));
            }
        }

        figment = figment.merge(Env::prefixed(ENV_PREFIX).split("__"));
        figment = figment.merge(overrides);

        let config: Self = figment
            .extract()
            .map_err(|e| FamiliarError::Config(e.to_string()))?;
        config.validate_repositories()?;
        config.validate_providers()?;
        config.validate_execution()?;
        config.validate_preflight()?;
        config.delivery.validate().map_err(FamiliarError::Config)?;
        config.worker.validate().map_err(FamiliarError::Config)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static BUDGET_ENV: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_are_sensible() {
        let config = Config::default();
        assert_eq!(config.daemon.heartbeat_interval_secs, 60);
        assert_eq!(config.logging.level, "info");
        assert_eq!(config.logging.format, LogFormat::Pretty);
        // inference defaults to disabled
        assert!(config.daemon.pid_file.is_none());
        assert!(config.daemon.socket_path.is_none());
        assert!(config.database.path.is_none());
        assert_eq!(config.execution_context.hard_ceiling_tokens, None);
        assert!(!config.review.enabled);
        assert_eq!(config.worker.max_prds_per_run, 1);
        assert_eq!(config.worker.restart_throttle_secs, 10);
        assert_eq!(config.review.max_review_attempts, 3);
    }

    #[test]
    fn legacy_driver_model_routes_name_registry_replacement() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            r#"
[[driver.model_routes]]
max_expected_files = 1
model = "legacy"
"#,
        )
        .unwrap();
        let error = Config::load(Some(file.path())).unwrap_err().to_string();
        assert!(error.contains("driver.model_routes"), "{error}");
        assert!(error.contains("worker_registry.routing.rules"), "{error}");
    }

    #[test]
    fn delivery_modes_are_explicit_and_fail_closed() {
        let repository = RepositoryConfig::default();
        assert!(repository
            .delivery_policy()
            .unwrap_err()
            .contains("missing"));
        let mut policy = DeliveryConfig {
            mode: DeliveryMode::PocSelfApproval,
            enabled: true,
            max_deliveries_per_session: 1,
            remote: "configured-remote".into(),
            base: "configured-base".into(),
            staging_environment: "staging".into(),
            provider_argv: vec!["adapter".into()],
            deploy_argv: vec!["deploy".into()],
            smoke_argv: vec!["health".into()],
            rollback_argv: vec!["rollback".into()],
            ..DeliveryConfig::default()
        };
        assert!(policy.validate().unwrap_err().contains("explicit warrant"));
        policy.mode = DeliveryMode::ReviewGatedAutomatic;
        assert!(policy.validate().unwrap_err().contains("implementer"));
    }

    #[test]
    fn persistent_worker_requires_finite_throttled_runs() {
        let mut worker = WorkerConfig {
            max_prds_per_run: 0,
            ..Default::default()
        };
        assert!(worker
            .validate()
            .unwrap_err()
            .contains("positive and finite"));
        worker.max_prds_per_run = 1;
        worker.restart_throttle_secs = 0;
        assert!(worker.validate().unwrap_err().contains("throttle"));
    }

    #[test]
    fn repository_profiles_validate_and_resolve_canonical_paths() {
        let repo = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.repositories.insert(
            repo.path().display().to_string(),
            RepositoryConfig {
                profile: "numbered-slug".into(),
                active_dir: "docs/prd/todo".into(),
                archived_dir: "docs/prd/done".into(),
                prd_metadata_policy: "incremental".into(),
                reference_roots: vec![],
                ..RepositoryConfig::default()
            },
        );
        config.validate_repositories().unwrap();
        let resolved = config
            .repository(&repo.path().canonicalize().unwrap())
            .unwrap();
        assert_eq!(
            resolved.layout().profile,
            crate::BacklogProfile::NumberedSlug
        );
    }

    /// PRD-065 defect-3 regression: a linked worktree resolves the policy of
    /// the configured main worktree through Git common-directory identity,
    /// with no path-specific configuration entry for the worktree itself.
    #[test]
    fn linked_worktree_resolves_policy_through_repository_identity() {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("main");
        std::fs::create_dir(&main).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir)
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "git {args:?}"
            );
        };
        git(&main, &["init", "-q"]);
        git(&main, &["config", "user.email", "test@example.invalid"]);
        git(&main, &["config", "user.name", "Test"]);
        std::fs::write(main.join("file"), "base").unwrap();
        git(&main, &["add", "file"]);
        git(&main, &["commit", "-qm", "base"]);
        let lease = temp.path().join("lease");
        git(
            &main,
            &["worktree", "add", "-q", lease.to_str().unwrap(), "HEAD"],
        );

        let mut config = Config::default();
        config.repositories.insert(
            main.display().to_string(),
            RepositoryConfig {
                profile: "numbered-slug".into(),
                active_dir: "docs/prd/todo".into(),
                archived_dir: "docs/prd/done".into(),
                prd_metadata_policy: "incremental".into(),
                reference_roots: vec![],
                ..RepositoryConfig::default()
            },
        );
        // The lease worktree has no entry of its own, yet resolves the main
        // worktree's policy.
        let resolved = config.repository(&lease.canonicalize().unwrap()).unwrap();
        assert_eq!(
            resolved.layout().profile,
            crate::BacklogProfile::NumberedSlug
        );
        // An unrelated repository still resolves the default.
        let other = temp.path().join("other");
        std::fs::create_dir(&other).unwrap();
        git(&other, &["init", "-q"]);
        let fallback = config.repository(&other.canonicalize().unwrap()).unwrap();
        assert_eq!(fallback.layout().profile, crate::BacklogProfile::Canonical);
        // Two entries naming the same repository identity fail closed with a
        // diagnostic naming both.
        config
            .repositories
            .insert(lease.display().to_string(), RepositoryConfig::default());
        // The exact-path match shadows identity resolution for the main
        // worktree itself, so probe from a third worktree of the same repo.
        let probe = temp.path().join("probe");
        git(
            &main,
            &["worktree", "add", "-q", probe.to_str().unwrap(), "HEAD"],
        );
        let error = config
            .repository(&probe.canonicalize().unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("same repository identity"), "{error}");
    }

    #[test]
    fn repository_profiles_fail_closed_on_invalid_shapes() {
        let repo = tempfile::tempdir().unwrap();
        for (profile, active, archived, expected) in [
            ("unknown", "todo", "done", "unknown backlog profile"),
            ("canonical", "../todo", "done", "traversal-free"),
            ("canonical", "/todo", "done", "traversal-free"),
            ("canonical", "same", "same", "must be distinct"),
        ] {
            let mut config = Config::default();
            config.repositories.insert(
                repo.path().display().to_string(),
                RepositoryConfig {
                    profile: profile.into(),
                    active_dir: active.into(),
                    archived_dir: archived.into(),
                    prd_metadata_policy: "incremental".into(),
                    reference_roots: vec![],
                    ..RepositoryConfig::default()
                },
            );
            assert!(config
                .validate_repositories()
                .unwrap_err()
                .to_string()
                .contains(expected));
        }
    }

    #[test]
    fn repository_execution_sections_resolve_wholesale_and_through_symlinks() {
        let repo = tempfile::tempdir().unwrap();
        let link_parent = tempfile::tempdir().unwrap();
        let link = link_parent.path().join("worktree-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(repo.path(), &link).unwrap();

        let mut config = Config::default();
        config.review.allowed_paths = vec!["global/".into()];
        config.execution_context.hard_ceiling_tokens = Some(10);
        let scoped_review = ReviewConfig {
            allowed_paths: vec!["scoped/".into()],
            max_review_attempts: 99,
            ..ReviewConfig::default()
        };
        config.repositories.insert(
            link.display().to_string(),
            RepositoryConfig {
                review: Some(scoped_review.clone()),
                execution_context: Some(ExecutionContextConfig {
                    hard_ceiling_tokens: Some(20),
                    ..ExecutionContextConfig::default()
                }),
                ..RepositoryConfig::default()
            },
        );
        config.validate_repositories().unwrap();
        let effective = config
            .effective_execution(&repo.path().canonicalize().unwrap())
            .unwrap();
        assert_eq!(effective.review, scoped_review);
        assert_eq!(effective.review_source, ConfigurationSource::Repository);
        assert_eq!(effective.execution_context.hard_ceiling_tokens, Some(20));
        assert_eq!(
            effective.execution_context_source,
            ConfigurationSource::Repository
        );

        let other = tempfile::tempdir().unwrap();
        let fallback = config
            .effective_execution(&other.path().canonicalize().unwrap())
            .unwrap();
        assert_eq!(fallback.review, config.review);
        assert_eq!(fallback.review_source, ConfigurationSource::Global);
        assert_eq!(fallback.execution_context, config.execution_context);
    }

    #[test]
    fn repository_execution_validation_and_closed_keys_fail_at_load() {
        let repo = tempfile::tempdir().unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            format!(
                "[repositories.\"{}\"]\nagents = {{}}\n",
                repo.path().display()
            ),
        )
        .unwrap();
        let unknown = Config::load(Some(file.path())).unwrap_err().to_string();
        assert!(
            unknown.contains("unknown field") && unknown.contains("agents"),
            "{unknown}"
        );

        std::fs::write(
            file.path(),
            format!(
                "[repositories.\"{}\".review]\nenabled = true\nmax_review_attempts = 0\n",
                repo.path().display()
            ),
        )
        .unwrap();
        let invalid = Config::load(Some(file.path())).unwrap_err().to_string();
        assert!(invalid.contains(&format!("repositories.{}.review", repo.path().display())));
        assert!(invalid.contains("finite positive review"), "{invalid}");
    }

    #[cfg(unix)]
    #[test]
    fn repository_profiles_refuse_duplicate_canonical_worktrees() {
        let parent = tempfile::tempdir().unwrap();
        let repo = parent.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        let mut config = Config::default();
        config
            .repositories
            .insert(repo.display().to_string(), RepositoryConfig::default());
        config
            .repositories
            .insert(link.display().to_string(), RepositoryConfig::default());
        assert!(config
            .validate_repositories()
            .unwrap_err()
            .to_string()
            .contains("resolve to the same worktree"));
    }

    #[test]
    fn load_without_file_succeeds() {
        // Note: actual values may differ from defaults if FAMILIAR_AI_ env vars are set
        let config = Config::load(None);
        assert!(config.is_ok());
    }

    #[test]
    fn load_from_toml_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
[logging]
level = "debug"
format = "json"
"#,
        )
        .unwrap();

        let config = Config::load(Some(tmp.path())).unwrap();
        assert_eq!(config.logging.level, "debug");
        assert_eq!(config.logging.format, LogFormat::Json);
    }

    #[test]
    fn execution_context_budget_is_optional_and_accepts_zero() {
        let _guard = BUDGET_ENV.lock().unwrap();
        for (source, expected) in [
            ("", None),
            ("[execution_context]\n", None),
            ("[execution_context]\nhard_ceiling_tokens = 0\n", Some(0)),
            (
                "[execution_context]\nhard_ceiling_tokens = 12000\n",
                Some(12000),
            ),
        ] {
            let tmp = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(tmp.path(), source).unwrap();
            let config = Config::load(Some(tmp.path())).unwrap();
            assert_eq!(config.execution_context.hard_ceiling_tokens, expected);
        }
    }

    #[test]
    fn invalid_execution_context_budget_fails_configuration() {
        let _guard = BUDGET_ENV.lock().unwrap();
        for value in ["-1", "18446744073709551616", "\"many\""] {
            let tmp = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(
                tmp.path(),
                format!("[execution_context]\nhard_ceiling_tokens = {value}\n"),
            )
            .unwrap();
            assert!(Config::load(Some(tmp.path())).is_err());
        }
    }

    #[test]
    fn loads_exact_model_execution_pricing() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
[execution_history.pricing."exact-model"]
input_microusd_per_million = 100
cached_input_microusd_per_million = 20
output_microusd_per_million = 300
"#,
        )
        .unwrap();
        let config = Config::load(Some(tmp.path())).unwrap();
        let price = &config.execution_history.pricing["exact-model"];
        assert_eq!(price.input_microusd_per_million, Some(100));
        assert_eq!(price.cached_input_microusd_per_million, Some(20));
        assert_eq!(price.output_microusd_per_million, Some(300));
    }

    #[test]
    fn env_overrides_file() {
        // Use INFERENCE__TEXT__MODE to test env override with new config
        let env_key = "FAMILIAR_AI_INFERENCE__TEXT__MODE";
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();

        std::env::set_var(env_key, "local_only");
        let config = Config::load(Some(tmp.path())).unwrap();
        std::env::remove_var(env_key);

        assert_eq!(config.inference.text.mode, InferenceMode::LocalOnly);
    }

    #[test]
    fn execution_context_budget_uses_existing_environment_mapping() {
        let _guard = BUDGET_ENV.lock().unwrap();
        let env_key = "FAMILIAR_AI_EXECUTION_CONTEXT__HARD_CEILING_TOKENS";
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "[execution_context]\nhard_ceiling_tokens = 10\n",
        )
        .unwrap();
        std::env::set_var(env_key, "42");
        let config = Config::load(Some(tmp.path())).unwrap();
        std::env::remove_var(env_key);
        assert_eq!(config.execution_context.hard_ceiling_tokens, Some(42));
    }

    #[test]
    fn load_with_overrides_takes_priority() {
        let overrides = Figment::from(Serialized::defaults(Config {
            logging: LoggingConfig {
                level: "trace".to_string(),
                ..Default::default()
            },
            ..Default::default()
        }));

        let config = Config::load_with_overrides(None, overrides).unwrap();
        assert_eq!(config.logging.level, "trace");
    }

    #[test]
    fn missing_config_file_does_not_error() {
        let config = Config::load(Some(Path::new("/nonexistent/config.toml")));
        assert!(config.is_ok());
    }

    #[test]
    fn enabled_review_requires_finite_attempts_and_a_resource_ceiling() {
        let mut review = ReviewConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(review.validate().is_err());
        review.max_total_duration_ms = 60_000;
        review.allowed_paths = vec!["src/".into()];
        review.verification = vec![ReviewVerificationConfig {
            check_id: "tests".into(),
            argv: vec!["cargo".into(), "test".into()],
            working_directory: ".".into(),
            timeout_ms: 1_000,
            required: true,
            path_prefixes: vec!["src/".into()],
            environment: BTreeMap::from([("PATH".into(), "/usr/bin".into())]),
        }];
        review.implementation_agent = ReviewAgentConfig {
            adapter_id: "fake".into(),
            agent_id: "implementation".into(),
            provider: None,
            model: None,
        };
        review.reviewer_agent = ReviewAgentConfig {
            adapter_id: "fake".into(),
            agent_id: "reviewer".into(),
            provider: None,
            model: None,
        };
        assert!(review.validate().is_ok());
        review.max_review_attempts = 0;
        assert!(review.validate().is_err());
    }

    fn valid_enabled_review() -> ReviewConfig {
        let mut review = ReviewConfig {
            enabled: true,
            ..Default::default()
        };
        review.max_total_duration_ms = 60_000;
        review.allowed_paths = vec!["src/".into()];
        review.verification = vec![ReviewVerificationConfig {
            check_id: "tests".into(),
            argv: vec!["/usr/bin/true".into()],
            working_directory: ".".into(),
            timeout_ms: 1_000,
            required: true,
            path_prefixes: vec![],
            environment: BTreeMap::new(),
        }];
        review.implementation_agent = ReviewAgentConfig {
            adapter_id: "fake".into(),
            agent_id: "implementation".into(),
            provider: None,
            model: None,
        };
        review.reviewer_agent = ReviewAgentConfig {
            adapter_id: "fake".into(),
            agent_id: "reviewer".into(),
            provider: None,
            model: None,
        };
        review
    }

    #[test]
    fn review_tier_rules_fail_closed_before_execution() {
        let mut review = valid_enabled_review();
        review.tier_policy = Some(ReviewTierPolicyConfig {
            independent_review_required: true,
            standard_reviewer_agent: ReviewAgentConfig::default(),
            full_review_risk_classes: vec![],
            rules: vec![ReviewTierRuleConfig {
                id: "tiny".into(),
                tier: ReviewTierConfig::ChecksOnly,
                path_prefixes: vec!["src/".into()],
                max_changed_files: Some(1),
                max_changed_bytes: None,
                change_kinds: vec!["modified".into()],
                scope_classes: vec![],
            }],
        });
        assert!(review
            .validate()
            .unwrap_err()
            .contains("independent review is required"));

        let policy = review.tier_policy.as_mut().unwrap();
        policy.independent_review_required = false;
        policy.rules.push(ReviewTierRuleConfig {
            id: "same".into(),
            tier: ReviewTierConfig::Full,
            path_prefixes: vec!["src/".into()],
            max_changed_files: Some(1),
            max_changed_bytes: None,
            change_kinds: vec!["modified".into()],
            scope_classes: vec![],
        });
        assert!(review.validate().unwrap_err().contains("contradict"));
    }

    #[test]
    fn full_review_risk_class_outside_repository_vocabulary_fails_validation() {
        let policy = ReviewTierPolicyConfig {
            independent_review_required: false,
            standard_reviewer_agent: ReviewAgentConfig::default(),
            full_review_risk_classes: vec!["unknown-class".into()],
            rules: vec![],
        };
        let vocabulary = BTreeSet::from(["review-policy"]);
        let error = policy.validate_risk_vocabulary(&vocabulary).unwrap_err();
        assert!(error.contains("unknown-class"));
        assert!(error.contains("outside the configured repository risk vocabulary"));
    }

    fn registry_worker(id: &str) -> RegistryWorkerConfig {
        RegistryWorkerConfig {
            adapter: AgentAdapterKind::Codex,
            provider: "openai".into(),
            model: id.into(),
            executable: None,
            capabilities: vec![WorkerCapabilityConfig::Implementation],
            fresh_process_isolation: true,
            context_tokens: 100,
            estimated_cost_microusd: 1,
            available: true,
            effort: None,
            permission_mode: None,
            extra_args: vec![],
        }
    }

    #[test]
    fn route_rule_naming_unknown_worker_fails_validation() {
        let mut registry = WorkerRegistryConfig {
            workers: BTreeMap::from([("codex".to_string(), registry_worker("codex"))]),
            routing: WorkerRoutingConfig::default(),
        };
        registry.routing.rules.push(WorkerRouteRuleConfig {
            id: "risky".into(),
            worker: "missing".into(),
            risk_classes: vec!["routing".into()],
            max_expected_files: None,
        });
        let vocabulary = std::collections::BTreeSet::from(["routing"]);
        assert!(registry
            .validate(&vocabulary)
            .unwrap_err()
            .contains("unknown worker"));
    }

    #[test]
    fn route_rule_naming_risk_class_outside_vocabulary_fails_validation() {
        let mut registry = WorkerRegistryConfig {
            workers: BTreeMap::from([("codex".to_string(), registry_worker("codex"))]),
            routing: WorkerRoutingConfig::default(),
        };
        registry.routing.rules.push(WorkerRouteRuleConfig {
            id: "risky".into(),
            worker: "codex".into(),
            risk_classes: vec!["unknown-class".into()],
            max_expected_files: None,
        });
        let vocabulary = std::collections::BTreeSet::from(["routing"]);
        assert!(registry
            .validate(&vocabulary)
            .unwrap_err()
            .contains("outside the configured vocabulary"));
        assert!(registry
            .validate(&std::collections::BTreeSet::new())
            .is_err());
    }

    #[test]
    fn route_rules_with_identical_predicates_and_different_workers_contradict() {
        let mut registry = WorkerRegistryConfig {
            workers: BTreeMap::from([
                ("codex".to_string(), registry_worker("codex")),
                ("claude".to_string(), registry_worker("claude")),
            ]),
            routing: WorkerRoutingConfig::default(),
        };
        registry.routing.rules.push(WorkerRouteRuleConfig {
            id: "first".into(),
            worker: "codex".into(),
            risk_classes: vec!["routing".into()],
            max_expected_files: None,
        });
        let vocabulary = std::collections::BTreeSet::from(["routing"]);
        assert!(registry.validate(&vocabulary).is_ok());

        registry.routing.rules.push(WorkerRouteRuleConfig {
            id: "second".into(),
            worker: "claude".into(),
            risk_classes: vec!["routing".into()],
            max_expected_files: None,
        });
        assert!(registry
            .validate(&vocabulary)
            .unwrap_err()
            .contains("contradict"));
    }

    #[test]
    fn legacy_prohibited_grammar_is_closed_and_lossless() {
        let path = ProhibitedChangeConfig::from("secrets/").resolve().unwrap();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].id, "legacy:path:secrets/");
        assert_eq!(path[0].path.as_deref(), Some("secrets/"));
        let glob = ProhibitedChangeConfig::from("secrets/**")
            .resolve()
            .unwrap();
        assert_eq!(glob[0].path.as_deref(), Some("secrets/"));
        let dependency = ProhibitedChangeConfig::from("dependency changes")
            .resolve()
            .unwrap();
        assert_eq!(
            dependency
                .iter()
                .map(|rule| (rule.id.as_str(), rule.class))
                .collect::<Vec<_>>(),
            vec![
                (
                    "legacy:class:dependency_manifest",
                    Some(ScopeFileClassName::DependencyManifest)
                ),
                (
                    "legacy:class:dependency_lockfile",
                    Some(ScopeFileClassName::DependencyLockfile)
                ),
            ]
        );
        for stale in ["commit", "push", "deployment", "no big rewrites"] {
            let error = ProhibitedChangeConfig::from(stale).resolve().unwrap_err();
            assert!(error.contains(stale), "diagnostic must name '{stale}'");
            assert!(error.contains("[[review.prohibited_changes]]"));
        }
    }

    #[test]
    fn typed_prohibited_rules_validate_fail_closed() {
        let valid = ProhibitedChangeConfig::Typed(TypedProhibitedChange {
            id: "no_migration_edits".into(),
            path: None,
            class: Some(ScopeFileClassName::Migration),
            change_kinds: vec!["modified".into(), "deleted".into()],
            description: None,
        });
        assert_eq!(valid.resolve().unwrap()[0].change_kinds.len(), 2);
        let both = ProhibitedChangeConfig::Typed(TypedProhibitedChange {
            id: "x".into(),
            path: Some("a".into()),
            class: Some(ScopeFileClassName::Test),
            change_kinds: vec![],
            description: None,
        });
        assert!(both.resolve().unwrap_err().contains("exactly one"));
        let bad_kind = ProhibitedChangeConfig::Typed(TypedProhibitedChange {
            id: "x".into(),
            path: Some("a".into()),
            class: None,
            change_kinds: vec!["committed".into()],
            description: None,
        });
        assert!(bad_kind.resolve().unwrap_err().contains("committed"));
    }

    #[test]
    fn enabled_review_scope_validation_is_fail_closed() {
        let mut review = valid_enabled_review();
        review.prohibited_changes = vec!["dependency changes".into()];
        assert!(review.validate().is_ok());
        review.prohibited_changes = vec!["push".into()];
        assert!(review
            .validate()
            .unwrap_err()
            .contains("closed legacy grammar"));
        review.prohibited_changes = vec!["secrets/".into(), "secrets/".into()];
        assert!(review.validate().unwrap_err().contains("duplicate"));
        let mut review = valid_enabled_review();
        review.allowed_paths = vec!["../outside".into()];
        assert!(review.validate().is_err());
        let mut review = valid_enabled_review();
        review.allowed_paths = vec![];
        assert!(review.validate().is_err());
        review.scope.allow_prd_expected_file_expansion = true;
        assert!(review.validate().is_ok());
        let mut review = valid_enabled_review();
        review.scope.classification = vec![ScopeClassificationConfig {
            id: "migrations".into(),
            class: ScopeFileClassName::Migration,
            path: "migrations/".into(),
            precedence: None,
        }];
        assert!(review.validate().is_ok());
        review.scope.classification.push(ScopeClassificationConfig {
            id: "migrations".into(),
            class: ScopeFileClassName::Configuration,
            path: "config/".into(),
            precedence: None,
        });
        assert!(review.validate().unwrap_err().contains("duplicate"));
    }

    #[test]
    fn scope_config_toml_round_trip_and_defaults() {
        let parsed: ReviewScopeConfig = toml::from_str(
            "allow_prd_expected_file_expansion = true\ndeclaration_mode = \"expected_required\"\n\n[file_classes]\ndependency_lockfile = \"deny\"\n",
        )
        .unwrap();
        assert!(parsed.allow_prd_expected_file_expansion);
        assert_eq!(
            parsed.declaration_mode,
            ScopeDeclarationModeConfig::ExpectedRequired
        );
        assert_eq!(
            parsed.file_classes.dependency_lockfile,
            ScopeClassPolicyConfig::Deny
        );
        assert_eq!(
            parsed.file_classes.dependency_manifest,
            ScopeClassPolicyConfig::HumanReview
        );
        assert_eq!(
            parsed.file_classes.test,
            ScopeClassPolicyConfig::AllowWhenExpected
        );
        let defaults = ReviewScopeConfig::default();
        assert!(!defaults.allow_prd_expected_file_expansion);
        assert_eq!(
            defaults.declaration_mode,
            ScopeDeclarationModeConfig::ExpectedOrConfigured
        );
    }

    #[test]
    fn prohibited_changes_toml_accepts_legacy_strings_and_typed_tables() {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            prohibited_changes: Vec<ProhibitedChangeConfig>,
        }
        let parsed: Wrapper =
            toml::from_str("prohibited_changes = [\"dependency changes\", \"secrets/\"]\n")
                .unwrap();
        assert_eq!(parsed.prohibited_changes.len(), 2);
        assert!(matches!(
            parsed.prohibited_changes[0],
            ProhibitedChangeConfig::Legacy(_)
        ));
        let parsed: Wrapper = toml::from_str(
            "[[prohibited_changes]]\nid = \"no_secrets\"\npath = \"secrets/\"\nchange_kinds = [\"added\"]\n",
        )
        .unwrap();
        match &parsed.prohibited_changes[0] {
            ProhibitedChangeConfig::Typed(rule) => {
                assert_eq!(rule.id, "no_secrets");
                assert_eq!(rule.change_kinds, vec!["added".to_owned()]);
            }
            other => panic!("expected typed rule, got {other:?}"),
        }
    }

    static AGENTS_ENV: Mutex<()> = Mutex::new(());

    #[test]
    fn agents_section_parses_canonical_toml_and_defaults_to_absent() {
        let _guard = AGENTS_ENV.lock().unwrap();
        assert!(Config::default().agents.is_none());
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();
        assert!(Config::load(Some(tmp.path())).unwrap().agents.is_none());
        std::fs::write(
            tmp.path(),
            "[agents.implementation]\nadapter = \"claude-code\"\nexecutable = \"claude\"\nmodel = \"sonnet\"\neffort = \"high\"\npermission_mode = \"acceptEdits\"\nmax_budget_microusd = 5\nextra_args = [\"--add-dir\", \"/tmp/x\"]\n\n[agents.reviewer]\nadapter = \"claude-code\"\npermission_mode = \"default\"\n",
        )
        .unwrap();
        let config = Config::load(Some(tmp.path())).unwrap();
        let agents = config.agents.unwrap();
        assert_eq!(agents.implementation.adapter, AgentAdapterKind::ClaudeCode);
        assert_eq!(agents.implementation.resolved_executable(), "claude");
        assert_eq!(agents.implementation.model.as_deref(), Some("sonnet"));
        assert_eq!(agents.implementation.effort, Some(AgentEffort::High));
        assert_eq!(
            agents.implementation.permission_mode,
            Some(AgentPermissionMode::AcceptEdits)
        );
        assert_eq!(agents.implementation.max_execution_cost_microusd, 5);
        assert_eq!(agents.implementation.extra_args.len(), 2);
        assert_eq!(
            agents.reviewer.permission_mode,
            Some(AgentPermissionMode::Default)
        );
        assert!(agents.validate(&ReviewConfig::default()).is_ok());
        // Codex entries resolve their executable by adapter default.
        assert_eq!(AgentEntryConfig::default().resolved_executable(), "codex");
    }

    #[test]
    fn planner_uses_agent_validation_and_positive_size_ceilings() {
        let parsed: Config = toml::from_str(
            "[planner]\nadapter='codex'\nmax_prds_per_batch=3\nmax_bytes_per_prd=4096\n",
        )
        .unwrap();
        assert_eq!(parsed.planner.as_ref().unwrap().max_prds_per_batch, 3);
        assert!(parsed.planner.as_ref().unwrap().validate().is_ok());
        let bad: Config =
            toml::from_str("[planner]\nadapter='codex'\neffort='high'\nmax_prds_per_batch=0\n")
                .unwrap();
        assert!(bad.planner.unwrap().validate().is_err());
    }

    #[test]
    fn agents_env_overrides_round_trip_through_existing_mapping() {
        let _guard = AGENTS_ENV.lock().unwrap();
        let env_key = "FAMILIAR_AI_AGENTS__IMPLEMENTATION__ADAPTER";
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "[agents.implementation]\nadapter = \"codex\"\n").unwrap();
        std::env::set_var(env_key, "claude-code");
        let config = Config::load(Some(tmp.path())).unwrap();
        std::env::remove_var(env_key);
        assert_eq!(
            config.agents.unwrap().implementation.adapter,
            AgentAdapterKind::ClaudeCode
        );
    }

    #[test]
    fn agents_validation_is_fail_closed() {
        let mut config: AgentsConfig = toml::from_str("").unwrap();
        assert!(config.validate(&ReviewConfig::default()).is_ok());
        // Unknown adapters fail at parse time (closed enum).
        assert!(
            toml::from_str::<AgentsConfig>("[implementation]\nadapter = \"cursor\"\n").is_err()
        );
        // Empty executable.
        config.implementation.executable = Some("  ".into());
        assert!(config.validate(&ReviewConfig::default()).is_err());
        // Effort/permission mode are claude-code only.
        let mut config = AgentsConfig::default();
        config.implementation.effort = Some(AgentEffort::Low);
        assert!(config
            .validate(&ReviewConfig::default())
            .unwrap_err()
            .contains("claude-code"));
        // Reviewer bypassPermissions is always rejected.
        let mut config = AgentsConfig::default();
        config.reviewer.adapter = AgentAdapterKind::ClaudeCode;
        config.reviewer.permission_mode = Some(AgentPermissionMode::BypassPermissions);
        assert!(config
            .validate(&ReviewConfig::default())
            .unwrap_err()
            .contains("bypassPermissions"));
        // Forbidden extra args: exact and =-joined forms.
        for arg in [
            "--resume",
            "--model=haiku",
            "--dangerously-skip-permissions",
        ] {
            let mut config = AgentsConfig::default();
            config.implementation.adapter = AgentAdapterKind::ClaudeCode;
            config.implementation.extra_args = vec![arg.into()];
            assert!(
                config.validate(&ReviewConfig::default()).is_err(),
                "extra arg {arg} must be rejected"
            );
        }
        // A non-forbidden arg passes.
        let mut config = AgentsConfig::default();
        config.implementation.adapter = AgentAdapterKind::ClaudeCode;
        config.implementation.extra_args = vec!["--add-dir".into(), "/tmp/x".into()];
        assert!(config.validate(&ReviewConfig::default()).is_ok());
    }

    #[test]
    fn agents_review_consistency_is_enforced_only_with_review_enabled() {
        let mut review = ReviewConfig::default();
        review.implementation_agent.adapter_id = "codex-cli".into();
        review.reviewer_agent.adapter_id = "codex".into();
        let agents = AgentsConfig::default();
        // Review disabled: no consistency requirement.
        assert!(agents.validate(&review).is_ok());
        review.enabled = true;
        let error = agents.validate(&review).unwrap_err();
        assert!(error.contains("contradicts"), "got: {error}");
        review.implementation_agent.adapter_id = "codex".into();
        assert!(agents.validate(&review).is_ok());
        // Model contradiction when both declare one.
        review.implementation_agent.model = Some("model-a".into());
        let mut agents = AgentsConfig::default();
        agents.implementation.model = Some("model-b".into());
        assert!(agents.validate(&review).unwrap_err().contains("model"));
        agents.implementation.model = Some("model-a".into());
        assert!(agents.validate(&review).is_ok());
    }

    #[test]
    fn stale_legacy_env_detection_is_exact_and_sorted() {
        // identity-gate exception: legacy-prefixed names are the test subject.
        let stale = stale_legacy_env(
            [
                "FAMILIAR_AI_DATABASE__PATH".to_owned(),
                "FAMILIAR_LOGGING__LEVEL".to_owned(), // identity-gate: allow
                "FAMILIAR_DATABASE__PATH".to_owned(), // identity-gate: allow
                "PATH".to_owned(),
                "FAMILIARITY".to_owned(),
            ]
            .into_iter(),
        );
        assert_eq!(
            stale,
            vec!["FAMILIAR_DATABASE__PATH", "FAMILIAR_LOGGING__LEVEL"] // identity-gate: allow
        );
        assert!(stale_legacy_env(
            ["FAMILIAR_AI_LOGGING__LEVEL".to_owned(), "HOME".to_owned()].into_iter()
        )
        .is_empty());
    }

    #[test]
    fn generated_repository_fragments_merge_additively_and_refuse_collisions() {
        let config_dir = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let main = config_dir.path().join("config.toml");
        std::fs::write(
            &main,
            format!(
                "[repositories.{:?}]\nprofile = \"canonical\"\n",
                first.path().display().to_string()
            ),
        )
        .unwrap();
        let fragments = config_dir.path().join("repositories");
        std::fs::create_dir(&fragments).unwrap();
        std::fs::write(
            fragments.join("second.toml"),
            format!(
                "[repositories.{:?}]\nprofile = \"canonical\"\n",
                second.path().display().to_string()
            ),
        )
        .unwrap();
        let loaded = Config::load(Some(&main)).unwrap();
        assert_eq!(loaded.repositories.len(), 2);

        std::fs::write(
            fragments.join("collision.toml"),
            format!(
                "[repositories.{:?}]\nprofile = \"canonical\"\n",
                first.path().display().to_string()
            ),
        )
        .unwrap();
        assert!(Config::load(Some(&main))
            .unwrap_err()
            .to_string()
            .contains("defined more than once"));
    }

    #[test]
    fn provider_validation_names_unknown_kind_and_malformed_host() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(
            &path,
            "[providers.test]\nkind = \"billing\"\nhost = \"localhost:1\"\nauth = \"none\"\n",
        )
        .unwrap();
        let error = Config::load(Some(&path)).unwrap_err().to_string();
        assert!(error.contains("billing"), "{error}");

        std::fs::write(
            &path,
            "[providers.test]\nkind = \"inference\"\nhost = \"https://bad/path\"\nauth = \"none\"\n",
        )
        .unwrap();
        let error = Config::load(Some(&path)).unwrap_err().to_string();
        assert!(error.contains("https://bad/path"), "{error}");
    }
}
