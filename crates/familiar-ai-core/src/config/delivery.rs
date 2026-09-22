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
    /// PRD-095. The identity this repository's authored commits and published
    /// pull requests carry. Absence is fail-closed at both boundaries: without
    /// it the daemon would author and publish under whatever ambient account
    /// its environment happens to supply, which on a host with more than one
    /// account is silently wrong until a write fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<DeliveryIdentityConfig>,
    /// PRD-097. Which forge grammar delivery speaks — `github`, `gitlab`,
    /// `gitea`, or `none`. Validated closed exactly as provider kinds are: an
    /// unknown identity fails deserialization rather than silently behaving
    /// like GitHub. `provider_argv` remains the executable/prefix override;
    /// this field selects the verb grammar itself.
    pub forge: Forge,
}

/// PRD-097. The forge a repository's delivery publishes, checks, and merges
/// through. `none` is a first-class adapter: delivery pushes the branch and
/// stops at a terminal phase naming the branch and base for a human to open
/// the request, which is a successful outcome, not a failure.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Forge {
    Github,
    Gitlab,
    Gitea,
    None,
}

/// PRD-095. A *descriptor* of who Familiar acts as — never a credential.
/// `docs/contracts/credential-authentication.md` keeps the secret side; this
/// carries only the names an operator could safely paste into a bug report.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeliveryIdentityConfig {
    /// Author and committer name for commits this repository authors.
    pub author_name: String,
    /// Author and committer email for commits this repository authors.
    pub author_email: String,
    /// The forge account publication must happen as. Compared against what
    /// `account_probe_argv` reports before publication is allowed to proceed.
    pub forge_account: String,
    /// Environment applied to provider-adapter invocations to select
    /// `forge_account` for that invocation alone. Per-invocation because the
    /// alternative — a global toggle such as `gh auth switch` — is a race
    /// under concurrent workers: two overlapping deliveries to
    /// differently-owned repositories would each publish under the other's
    /// account depending on scheduling.
    #[serde(default)]
    pub provider_env: BTreeMap<String, String>,
    /// Argv that prints the forge account the adapter actually resolves to,
    /// on stdout, alone (for the GitHub CLI: `gh api user --jq .login`). Run
    /// as a preflight so a wrong identity costs a diagnostic rather than a
    /// half-finished delivery.
    pub account_probe_argv: Vec<String>,
}

/// Environment names that may not appear in `provider_env`. Identity
/// selection is a descriptor, so anything shaped like a bearer secret is
/// refused outright rather than trusted to be harmless: a token placed here
/// would reach process arguments and diagnostics, which the credential
/// contract forbids.
fn is_secret_shaped(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["TOKEN", "SECRET", "PASSWORD", "PASSPHRASE", "CREDENTIAL"]
        .iter()
        .any(|needle| upper.contains(needle))
        || upper.ends_with("_KEY")
        || upper == "KEY"
}

/// PRD-095. Whether an argv changes identity *outside* the invocation — the
/// thing per-invocation selection exists to avoid. Names the offending shape
/// so the diagnostic can say which rule was hit rather than only that one was.
pub fn global_identity_mutation(argv: &[String]) -> Option<&'static str> {
    let lowered: Vec<String> = argv.iter().map(|a| a.to_ascii_lowercase()).collect();
    let has = |needle: &str| lowered.iter().any(|a| a == needle);
    if has("auth") && (has("switch") || has("login") || has("logout")) {
        return Some("switches the adapter's active account");
    }
    if has("config") && (has("--global") || has("--system")) {
        return Some("writes global or system git configuration");
    }
    None
}

impl DeliveryIdentityConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.author_name.trim().is_empty() || self.author_email.trim().is_empty() {
            return Err(
                "delivery identity requires a non-empty author_name and author_email".into(),
            );
        }
        if self.forge_account.trim().is_empty() {
            return Err("delivery identity requires a non-empty forge_account".into());
        }
        if self.account_probe_argv.is_empty() {
            return Err(
                "delivery identity requires account_probe_argv so the published account can be \
                 verified before publication"
                    .into(),
            );
        }
        if let Some(reason) = global_identity_mutation(&self.account_probe_argv) {
            return Err(format!(
                "delivery identity account_probe_argv mutates machine-global identity ({reason}); \
                 the probe must only report, never change, the active account"
            ));
        }
        for name in self.provider_env.keys() {
            if name.trim().is_empty() {
                return Err("delivery identity provider_env names must be non-empty".into());
            }
            if is_secret_shaped(name) {
                return Err(format!(
                    "delivery identity provider_env may not carry {name:?}: identity is a \
                     descriptor, and credentials must not appear in configuration or process \
                     arguments"
                ));
            }
        }
        Ok(())
    }
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
    #[serde(default)]
    identity: Option<DeliveryIdentityConfig>,
    #[serde(default)]
    forge: Option<Forge>,
}

impl From<DeliveryConfigCompat> for DeliveryConfig {
    fn from(compat: DeliveryConfigCompat) -> Self {
        let mode = compat.mode.unwrap_or(if compat.enabled {
            default_delivery_mode()
        } else {
            DeliveryMode::Disabled
        });
        // PRD-097: an omitted `forge` defaults to `github`, the pre-PRD-097
        // baseline, whether or not `provider_argv` happens to be populated —
        // inferring `none` from an empty `provider_argv` made a missing or
        // mistyped adapter executable validate as an intentional "no forge"
        // section instead of failing closed. `Forge::None` is reserved for a
        // section that spells `forge = "none"` explicitly.
        let forge = compat.forge.unwrap_or(Forge::Github);
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
            identity: compat.identity,
            forge,
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
            identity: None,
            // Matches the compat `From` impl's default for an omitted
            // `forge`: GitHub is the pre-PRD-097 baseline.
            forge: Forge::Github,
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
        if self.forge != Forge::None && self.provider_argv.is_empty() {
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
        if let Some(identity) = &self.identity {
            identity.validate()?;
        }
        // PRD-095: a configured argv may not toggle machine-global identity.
        // Selecting an account globally is a race under concurrent workers —
        // two overlapping deliveries would each publish under the other's
        // account depending on scheduling — so it is refused in configuration
        // rather than merely avoided in the delivery path.
        for command in [
            &self.provider_argv,
            &self.deploy_argv,
            &self.smoke_argv,
            &self.rollback_argv,
            &self.migration_gate_argv,
        ] {
            if let Some(reason) = global_identity_mutation(command) {
                return Err(format!(
                    "delivery argv {command:?} mutates machine-global identity ({reason}); \
                     identity must be selected per invocation"
                ));
            }
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
