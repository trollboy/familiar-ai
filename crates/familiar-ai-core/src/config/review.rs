use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::default_true;

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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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
    /// Declared PRD risk classes explicitly opted into the provider batch
    /// interface (PRD-071). Batch tiering defaults off: an empty list here
    /// (the default) never routes any review through the batch interface.
    /// A class named both here and in `full_review_risk_classes` still
    /// receives full review — the audit trail below records the mutation,
    /// never a silent latency downgrade for a high-risk class.
    #[serde(default)]
    pub batch_risk_classes: Vec<String>,
    /// Maximum wait, in milliseconds, before a parked batch review falls
    /// back to the interactive tier. Required (non-zero) whenever
    /// `batch_risk_classes` is non-empty so batch is always a bounded
    /// discount, never an unbounded stall.
    #[serde(default)]
    pub max_batch_wait_ms: u64,
    /// Names a `[worker_registry.workers.<id>]` entry with
    /// `runtime = "anthropic-api"` that submissions use as the batch
    /// transport. Required whenever `batch_risk_classes` is non-empty;
    /// cross-referenced against the worker registry at config load
    /// (`Config::validate`), not merely at submission time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_worker: Option<String>,
    #[serde(default)]
    pub rules: Vec<ReviewTierRuleConfig>,
}

impl ReviewTierPolicyConfig {
    pub(super) fn validate_risk_vocabulary(
        &self,
        risk_vocabulary: &BTreeSet<&str>,
    ) -> Result<(), String> {
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
        let mut seen_batch = BTreeSet::new();
        for class in &self.batch_risk_classes {
            if class.trim().is_empty() || class.trim() != class {
                return Err(
                    "tier_policy.batch_risk_classes entries must be non-empty and trimmed".into(),
                );
            }
            if !seen_batch.insert(class.as_str()) {
                return Err(format!(
                    "tier_policy.batch_risk_classes contains duplicate class '{class}'"
                ));
            }
            if !risk_vocabulary.contains(class.as_str()) {
                return Err(format!(
                    "tier_policy.batch_risk_classes names risk class '{class}' outside the configured repository risk vocabulary"
                ));
            }
        }
        if !self.batch_risk_classes.is_empty() && self.max_batch_wait_ms == 0 {
            return Err(
                "tier_policy.batch_risk_classes requires a positive tier_policy.max_batch_wait_ms bound"
                    .into(),
            );
        }
        if !self.batch_risk_classes.is_empty()
            && !self
                .batch_worker
                .as_deref()
                .is_some_and(|worker| !worker.is_empty())
        {
            return Err(
                "tier_policy.batch_risk_classes requires tier_policy.batch_worker naming a worker_registry entry"
                    .into(),
            );
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
