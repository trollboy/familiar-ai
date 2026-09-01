use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

use crate::FamiliarError;

mod accounting;
mod agent_runtime;
mod artifacts;
mod compression;
mod context_service;
mod daemon;
mod dashboard;
mod delivery;
mod driver;
mod inference;
mod packer;
mod preflight;
mod project_toml;
mod providers;
mod registry_workers;
mod repository;
mod review;
mod rollup;
mod summary;
mod supervised_worker;
mod tray;
mod watcher;

pub use accounting::*;
pub use agent_runtime::*;
pub use artifacts::*;
pub use compression::*;
pub use context_service::*;
pub use daemon::*;
pub use dashboard::*;
pub use delivery::*;
pub use driver::*;
pub use inference::*;
pub use packer::*;
pub use preflight::*;
pub use project_toml::*;
pub use providers::*;
pub use registry_workers::*;
pub use repository::*;
pub use review::*;
pub use rollup::*;
pub use summary::*;
pub use supervised_worker::*;
pub use tray::*;
pub use watcher::*;

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
    /// Immutable local model artifacts. Keys are friendly aliases only; the
    /// content-derived id remains the routing and history partition key.
    #[serde(default)]
    pub artifacts: BTreeMap<String, ModelArtifactConfig>,
    /// Operator-managed authentication/entitlement references. Values are
    /// diagnostic descriptors only; credential bytes never enter config.
    #[serde(default)]
    pub auth_profiles: BTreeMap<String, AuthDescriptor>,
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
    /// PRD-053 cost-reconciliation tolerance and settlement horizon.
    #[serde(default)]
    pub reconciliation: ReconciliationConfig,
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
    /// Native compression is inert unless an identity is explicitly selected.
    #[serde(default)]
    pub compression: CompressionConfig,
    /// PRD-058 Familiar-owned raw-model agent loop. Absent/disabled changes
    /// no existing harness-driven execution behavior.
    #[serde(default)]
    pub agent_runtime: AgentRuntimeConfig,
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
    /// Validate the complete effective configuration. Mutation callers use
    /// this same boundary as startup before exposing new bytes.
    pub fn validate(&self) -> crate::Result<()> {
        self.validate_repositories()?;
        self.validate_providers()?;
        self.validate_artifacts()?;
        self.validate_execution()?;
        self.validate_preflight()?;
        if self.daemon.global_concurrency_ceiling == 0
            || self.daemon.default_project_concurrency_ceiling == 0
            || self.daemon.health_timeout_ms == 0
        {
            return Err(FamiliarError::Config(
                "daemon control-plane ceilings and health_timeout_ms must be greater than zero"
                    .into(),
            ));
        }
        self.delivery.validate().map_err(FamiliarError::Config)?;
        self.worker.validate().map_err(FamiliarError::Config)?;
        self.compression
            .validate(&self.providers)
            .map_err(FamiliarError::Config)?;
        self.agent_runtime
            .validate()
            .map_err(FamiliarError::Config)?;
        Ok(())
    }

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
            for (id, worker) in &registry.workers {
                if let Some(profile) = &worker.auth_profile {
                    if !self.auth_profiles.contains_key(profile) {
                        return Err(FamiliarError::Config(format!(
                            "worker_registry.workers.{id} auth profile '{profile}' is missing; configure [auth_profiles.{profile}] with a BYO-Auth descriptor"
                        )));
                    }
                }
            }
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
    fn validate_artifacts(&self) -> crate::Result<()> {
        for (alias, artifact) in &self.artifacts {
            validate_identifier(alias, "artifact alias").map_err(FamiliarError::Config)?;
            let valid_id = artifact.id.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            });
            if !valid_id {
                return Err(FamiliarError::Config(format!(
                    "artifact '{alias}' has an invalid immutable id"
                )));
            }
            match artifact.state {
                ArtifactVerificationState::Verified => {
                    let manifest = artifact.manifest.as_ref().ok_or_else(|| {
                        FamiliarError::Config(format!(
                            "verified artifact '{alias}' lacks a manifest"
                        ))
                    })?;
                    if manifest.schema_version != ARTIFACT_MANIFEST_SCHEMA
                        || manifest.digest_algorithm != ARTIFACT_DIGEST_ALGORITHM
                    {
                        return Err(FamiliarError::Config(format!(
                            "artifact '{alias}' uses an unsupported manifest version"
                        )));
                    }
                    let encoded = serde_json::to_vec(manifest)
                        .map_err(|error| FamiliarError::Config(error.to_string()))?;
                    let actual = format!(
                        "sha256:{}",
                        hex_digest(ring::digest::digest(&ring::digest::SHA256, &encoded).as_ref())
                    );
                    if actual != artifact.id {
                        return Err(FamiliarError::Config(format!(
                            "artifact '{alias}' id does not match its canonical manifest"
                        )));
                    }
                }
                ArtifactVerificationState::DegradedUnverifiedAlias => {
                    if artifact
                        .runtime_alias
                        .as_deref()
                        .map_or(true, str::is_empty)
                        || artifact.manifest.is_some()
                    {
                        return Err(FamiliarError::Config(format!(
                            "degraded artifact '{alias}' requires only a runtime alias"
                        )));
                    }
                }
            }
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
        config.validate()?;
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
        config
            .compression
            .validate(&config.providers)
            .map_err(FamiliarError::Config)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static BUDGET_ENV: Mutex<()> = Mutex::new(());

    #[test]
    fn project_config_is_closed_and_rejects_dangerous_values() {
        assert!(FamiliarToml::parse("token = \"sk-secret\"").is_err());
        assert!(FamiliarToml::parse("active_dir = \"/tmp/prds\"").is_err());
        assert!(FamiliarToml::parse(
            "[environments.prod]\nrequires='deploy-target'\nname='prod'\nhost='evil.example'"
        )
        .is_err());
        assert!(
            FamiliarToml::parse("[environments.prod]\nrequires='deploy-target'\nname='prod'")
                .is_ok()
        );
    }

    #[test]
    fn project_config_overlays_only_declared_shareable_values() {
        let project = FamiliarToml::parse("profile='numbered-slug'\nrisk_vocabulary=['security']\n[environments.prod]\nrequires='deploy-target'\nname='production'").unwrap();
        let mut machine = RepositoryConfig::default();
        machine
            .bindings
            .insert("production".into(), "local-provider".into());
        machine.delivery = Some(DeliveryConfig::default());
        let effective = project.repository_config(&machine);
        assert_eq!(effective.profile, "numbered-slug");
        assert_eq!(
            effective.bindings.get("production").unwrap(),
            "local-provider"
        );
        assert_eq!(effective.delivery, machine.delivery);
    }

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

    /// FAM-BUG-023 regression: a legacy `[delivery]` table with only
    /// `enabled = false` deserializes to disabled mode; an explicit mode
    /// always wins; an empty table is disabled (fail closed).
    #[test]
    fn legacy_disabled_delivery_deserializes_to_disabled_mode() {
        let from =
            |json: serde_json::Value| -> DeliveryConfig { serde_json::from_value(json).unwrap() };
        assert_eq!(
            from(serde_json::json!({"enabled": false})).mode,
            DeliveryMode::Disabled
        );
        assert_eq!(from(serde_json::json!({})).mode, DeliveryMode::Disabled);
        assert_eq!(
            from(serde_json::json!({"enabled": true})).mode,
            DeliveryMode::ReviewedPrManual
        );
        assert_eq!(
            from(serde_json::json!({"enabled": false, "mode": "reviewed_pr_manual"})).mode,
            DeliveryMode::ReviewedPrManual
        );
        // The historical bug shape validates instead of demanding delivery
        // fields the operator never configured.
        assert!(from(serde_json::json!({"enabled": false}))
            .validate()
            .is_ok());
    }

    #[test]
    fn capability_display_spelling_round_trips_with_serde() {
        for capability in [
            WorkerCapabilityConfig::Planning,
            WorkerCapabilityConfig::Implementation,
            WorkerCapabilityConfig::Review,
            WorkerCapabilityConfig::Remediation,
            WorkerCapabilityConfig::NarrowTask,
        ] {
            let serialized = serde_json::to_value(capability).unwrap();
            assert_eq!(
                serialized.as_str().unwrap(),
                capability.as_str(),
                "display spelling must match the canonical serialized form"
            );
        }
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
            adapter: Some(AgentAdapterKind::Codex),
            provider: "openai".into(),
            model: id.into(),
            runtime: None,
            model_artifact: None,
            auth_profile: None,
            capability_profile: None,
            runtime_config: None,
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

    #[test]
    fn unsloth_is_a_typed_inference_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(
            &path,
            "[providers.local]\nkind = \"inference\"\nruntime = \"unsloth\"\nhost = \"127.0.0.1:8888\"\nauth = \"env: UNSLOTH_API_KEY\"\nmodels = [\"unsloth/Qwen3.8-27B-GGUF:UD-Q4_K_XL\"]\n",
        )
        .unwrap();
        let config = Config::load(Some(&path)).unwrap();
        assert_eq!(
            config.providers["local"].runtime,
            Some(InferenceRuntimeKind::Unsloth)
        );
    }

    #[test]
    fn legacy_registry_migration_and_aliases_are_deterministic() {
        let mut first = registry_worker("same-model");
        first.provider = "openai".into();
        assert_eq!(first.runtime_id().unwrap(), "codex");
        assert_eq!(
            first.canonical_spec_identity().unwrap(),
            first.canonical_spec_identity().unwrap()
        );
        let mut registry = WorkerRegistryConfig {
            workers: BTreeMap::from([("harness".into(), first.clone())]),
            ..Default::default()
        };
        assert_eq!(
            registry
                .resolve_worker("openai/same-model")
                .unwrap()
                .runtime_id()
                .unwrap(),
            "codex"
        );
        let mut raw = first;
        raw.adapter = None;
        raw.runtime = Some("openai-api".into());
        registry.workers.insert("raw-api".into(), raw);
        let error = registry.resolve_worker("openai/same-model").unwrap_err();
        assert!(
            error.contains("ambiguous") && error.contains("harness") && error.contains("raw-api"),
            "{error}"
        );
    }

    #[test]
    fn runtime_artifact_and_capability_profile_partition_identity() {
        let base = registry_worker("qwen");
        let mut runtime = base.clone();
        runtime.adapter = None;
        runtime.runtime = Some("ollama".into());
        assert_ne!(
            base.canonical_spec_identity().unwrap(),
            runtime.canonical_spec_identity().unwrap()
        );
        let mut artifact = runtime.clone();
        artifact.model_artifact = Some(format!("sha256:{}", "a".repeat(64)));
        assert_ne!(
            runtime.canonical_spec_identity().unwrap(),
            artifact.canonical_spec_identity().unwrap()
        );
        artifact.capability_profile = Some("different".into());
        assert_ne!(
            runtime.canonical_spec_identity().unwrap(),
            artifact.canonical_spec_identity().unwrap()
        );
    }

    #[test]
    fn worker_extensions_and_missing_auth_fail_closed_with_remedies() {
        let mut worker = registry_worker("model");
        worker.auth_profile = Some("missing-login".into());
        worker.runtime_config = Some(OllamaRuntimeConfig::default());
        let registry = WorkerRegistryConfig {
            workers: BTreeMap::from([("entry".into(), worker)]),
            ..Default::default()
        };
        let error = registry.validate(&BTreeSet::new()).unwrap_err();
        assert!(
            error.contains("entry") && error.contains("ollama"),
            "{error}"
        );
        assert!(toml::from_str::<RegistryWorkerConfig>("runtime='codex'\nprovider='openai'\nmodel='runtime-selected'\ncapabilities=['implementation']\n[runtime_config]\nunknown=true").is_err());
    }
}
