use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::validate_identifier;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, from = "DeliveryConfigCompat")]
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

/// FAM-BUG-023: a legacy `[delivery]` table carrying only `enabled = false`
/// must deserialize to disabled mode, not to the reviewed-PR default that
/// then demands delivery fields the operator never configured. When `mode`
/// is absent, the legacy `enabled` flag decides between the reviewed-PR
/// default and disabled; an explicit `mode` always wins.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryConfigCompat {
    #[serde(default)]
    mode: Option<DeliveryMode>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    max_deliveries_per_session: u64,
    #[serde(default = "default_delivery_command_timeout_ms")]
    command_timeout_ms: u64,
    #[serde(default = "default_delivery_remote")]
    remote: String,
    #[serde(default = "default_delivery_base")]
    base: String,
    #[serde(default)]
    provider_argv: Vec<String>,
    #[serde(default)]
    auto_merge: bool,
    #[serde(default)]
    staging_environment: String,
    #[serde(default)]
    deploy_argv: Vec<String>,
    #[serde(default)]
    smoke_argv: Vec<String>,
    #[serde(default)]
    rollback_argv: Vec<String>,
    #[serde(default)]
    comment_blockers: bool,
    #[serde(default)]
    required_checks: Vec<String>,
    #[serde(default)]
    migration_gate_argv: Vec<String>,
    #[serde(default)]
    credential_references: Vec<String>,
    #[serde(default)]
    poc_warrant: Option<PocSelfApprovalWarrant>,
    #[serde(default)]
    review_gate: Option<ReviewGateConfig>,
    #[serde(default)]
    targets: BTreeMap<String, String>,
}

impl From<DeliveryConfigCompat> for DeliveryConfig {
    fn from(compat: DeliveryConfigCompat) -> Self {
        let mode = compat.mode.unwrap_or(if compat.enabled {
            default_delivery_mode()
        } else {
            DeliveryMode::Disabled
        });
        Self {
            mode,
            enabled: compat.enabled,
            max_deliveries_per_session: compat.max_deliveries_per_session,
            command_timeout_ms: compat.command_timeout_ms,
            remote: compat.remote,
            base: compat.base,
            provider_argv: compat.provider_argv,
            auto_merge: compat.auto_merge,
            staging_environment: compat.staging_environment,
            deploy_argv: compat.deploy_argv,
            smoke_argv: compat.smoke_argv,
            rollback_argv: compat.rollback_argv,
            comment_blockers: compat.comment_blockers,
            required_checks: compat.required_checks,
            migration_gate_argv: compat.migration_gate_argv,
            credential_references: compat.credential_references,
            poc_warrant: compat.poc_warrant,
            review_gate: compat.review_gate,
            targets: compat.targets,
        }
    }
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
