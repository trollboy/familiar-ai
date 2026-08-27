use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

use crate::FamiliarError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub repositories: BTreeMap<String, RepositoryConfig>,
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
    #[serde(default)]
    pub driver: DriverConfig,
    #[serde(default)]
    pub worker: WorkerConfig,
    #[serde(default)]
    pub preflight: PreflightConfig,
    #[serde(default)]
    pub delivery: DeliveryConfig,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeliveryConfig {
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
}

fn default_delivery_remote() -> String {
    "origin".into()
}

fn default_delivery_base() -> String {
    "main".into()
}

fn default_delivery_command_timeout_ms() -> u64 {
    1_800_000
}

impl DeliveryConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
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
        if self.staging_environment != "staging" {
            return Err("delivery staging_environment must be exactly 'staging'".into());
        }
        if self.deploy_argv.is_empty()
            || self.smoke_argv.is_empty()
            || self.rollback_argv.is_empty()
        {
            return Err("delivery requires deploy, smoke, and rollback argv for staging".into());
        }
        Ok(())
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_context: Option<ExecutionContextConfig>,
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
            review: None,
            execution_context: None,
        }
    }
}

impl RepositoryConfig {
    pub fn layout(&self) -> crate::BacklogLayout {
        crate::BacklogLayout {
            profile: crate::BacklogProfile::parse(&self.profile).expect("validated profile"),
            active_dir: crate::RepositoryPath::new(self.active_dir.clone())
                .expect("validated active_dir"),
            archived_dir: crate::RepositoryPath::new(self.archived_dir.clone())
                .expect("validated archived_dir"),
            metadata_policy: crate::PrdMetadataPolicy::parse(&self.prd_metadata_policy)
                .expect("validated prd_metadata_policy"),
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
    /// Ordered deterministic implementation routes. The first route whose
    /// maximum scope count covers a PRD wins; no inference call selects it.
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
        if self.max_concurrency > 1 && !self.isolated_worktrees {
            return Err(
                "driver.isolated_worktrees must be true when max_concurrency is greater than one"
                    .into(),
            );
        }
        let mut prior = 0;
        for (index, route) in self.model_routes.iter().enumerate() {
            if route.max_expected_files == 0 || route.model.trim().is_empty() {
                return Err(format!(
                    "driver.model_routes[{index}] requires a positive max_expected_files and non-empty model"
                ));
            }
            if index > 0 && route.max_expected_files <= prior {
                return Err(
                    "driver.model_routes must be ordered by increasing max_expected_files".into(),
                );
            }
            prior = route.max_expected_files;
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
}

impl AgentAdapterKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
        }
    }
    pub fn default_executable(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
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
        if self.adapter == AgentAdapterKind::Codex
            && (self.effort.is_some() || self.permission_mode.is_some())
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
            if let Some(review) = &entry.review {
                review.validate().map_err(|error| {
                    FamiliarError::Config(format!("repositories.{worktree}.review: {error}"))
                })?;
                if let Some(agents) = &self.agents {
                    agents.validate(review).map_err(|error| {
                        FamiliarError::Config(format!("repositories.{worktree}.review: {error}"))
                    })?;
                }
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

    pub fn repository(&self, canonical_worktree: &Path) -> RepositoryConfig {
        self.repositories
            .iter()
            .find_map(|(path, entry)| {
                Path::new(path)
                    .canonicalize()
                    .ok()
                    .filter(|p| p == canonical_worktree)
                    .map(|_| entry.clone())
            })
            .unwrap_or_default()
    }

    pub fn effective_execution(&self, canonical_worktree: &Path) -> EffectiveExecutionConfig {
        let entry = self.repositories.iter().find_map(|(path, entry)| {
            Path::new(path)
                .canonicalize()
                .ok()
                .filter(|path| path == canonical_worktree)
                .map(|_| entry)
        });
        EffectiveExecutionConfig {
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
        }
    }

    fn validate_execution(&self) -> crate::Result<()> {
        self.review.validate().map_err(FamiliarError::Config)?;
        if let Some(agents) = &self.agents {
            agents
                .validate(&self.review)
                .map_err(FamiliarError::Config)?;
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

        figment = figment.merge(Env::prefixed(ENV_PREFIX).split("__"));

        let config: Self = figment
            .extract()
            .map_err(|e| FamiliarError::Config(e.to_string()))?;
        config.validate_repositories()?;
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
    fn persistent_worker_requires_finite_throttled_runs() {
        let mut worker = WorkerConfig::default();
        worker.max_prds_per_run = 0;
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
        let resolved = config.repository(&repo.path().canonicalize().unwrap());
        assert_eq!(
            resolved.layout().profile,
            crate::BacklogProfile::NumberedSlug
        );
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
        let effective = config.effective_execution(&repo.path().canonicalize().unwrap());
        assert_eq!(effective.review, scoped_review);
        assert_eq!(effective.review_source, ConfigurationSource::Repository);
        assert_eq!(effective.execution_context.hard_ceiling_tokens, Some(20));
        assert_eq!(
            effective.execution_context_source,
            ConfigurationSource::Repository
        );

        let other = tempfile::tempdir().unwrap();
        let fallback = config.effective_execution(&other.path().canonicalize().unwrap());
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
}
