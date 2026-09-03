//! `familiar-ai stewardship workers` — the worker inventory diagnostic.
//!
//! Discovery, registration, enablement, capability verification, and routing
//! readiness are separate states. Reporting only "registered" let operators
//! (and Familiar) describe machine capability incorrectly: a worker could be
//! configured and enabled while its capabilities were merely *declared* and
//! its model identity synthetic (FAM-BUG-001, FAM-BUG-009,
//! FAM-FRICTION-001/002/003). This prints every configured candidate with
//! each state typed, and — for anything not routable — the exact command
//! that would advance it.

use familiar_ai_core::config::{CapabilityProvenanceConfig, Config, RegistryWorkerConfig};
use serde::Serialize;

/// Why a configured worker cannot currently be routed to. Empty means
/// routable.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Blocker {
    /// Operator has not enabled it.
    Disabled,
    /// No capability is recorded at all — routing would be a guess.
    NoCapabilities,
    /// Every capability is operator-asserted; nothing has been probed or
    /// observed. Declared capability is a claim, not evidence.
    CapabilitiesDeclaredOnly,
    /// The model identity repeats the executable/CLI label, which is what a
    /// synthetic discovery probe records rather than a real model — the
    /// shape that consumed a whole wave in FAM-BUG-009.
    SyntheticModelIdentity,
    /// Cost has never been measured, so cost-based routing cannot rank it
    /// (FAM-BUG-007). Not fatal: it routes, it just cannot be compared.
    CostUnmeasured,
}

impl Blocker {
    /// The exact command that advances this transition, or `None` when the
    /// state is informational rather than blocking.
    pub fn remediation(&self, worker_id: &str) -> Option<String> {
        match self {
            Self::Disabled => Some(format!(
                "familiar-ai config model enable {worker_id}   # or set available = true"
            )),
            Self::NoCapabilities | Self::CapabilitiesDeclaredOnly => Some(format!(
                "familiar-ai preflight   # probes {worker_id} and records observed capability"
            )),
            Self::SyntheticModelIdentity => Some(format!(
                "set an explicit model for {worker_id} in [worker_registry.workers.{worker_id}] \
                 — the CLI's own name is not a model identity"
            )),
            Self::CostUnmeasured => None,
        }
    }

    fn is_fatal(&self) -> bool {
        !matches!(self, Self::CostUnmeasured)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerInventoryRow {
    pub worker_id: String,
    pub provider: String,
    pub model: String,
    pub runtime: Option<String>,
    pub enabled: bool,
    /// Capability name to the strongest provenance recorded for it.
    pub capabilities: Vec<(String, String)>,
    pub cost_microusd: Option<u64>,
    pub routable: bool,
    pub blockers: Vec<Blocker>,
    pub remediation: Vec<String>,
}

/// Whether a model identity is the CLI's own label rather than a model.
fn synthetic_model_identity(worker: &RegistryWorkerConfig) -> bool {
    let model = worker.model.trim();
    if model.is_empty() {
        return true;
    }
    let executable = worker
        .executable
        .as_deref()
        .map(|value| {
            value
                .rsplit('/')
                .next()
                .unwrap_or(value)
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    let lowered = model.to_ascii_lowercase();
    lowered == executable || lowered == worker.provider.trim().to_ascii_lowercase()
}

pub fn inventory(config: &Config) -> Vec<WorkerInventoryRow> {
    let Some(registry) = config.worker_registry.as_ref() else {
        return Vec::new();
    };
    registry
        .workers
        .iter()
        .map(|(id, worker)| {
            // Provenance lives in the referenced capability profile; a
            // worker with no profile has only its declared capability list,
            // which is a claim rather than evidence.
            let profile = worker
                .capability_profile
                .as_deref()
                .and_then(|name| registry.capability_profiles.get(name));
            let capabilities: Vec<(String, String)> = match profile {
                Some(profile) => profile
                    .capabilities
                    .iter()
                    .map(|(capability, provenance)| {
                        (
                            // RuntimeCapabilityConfig has no as_str(); its serde
                            // representation is the canonical name.
                            serde_json::to_string(capability)
                                .unwrap_or_default()
                                .trim_matches('"')
                                .to_owned(),
                            match provenance {
                                CapabilityProvenanceConfig::Declared => "declared",
                                CapabilityProvenanceConfig::Probed => "probed",
                                CapabilityProvenanceConfig::Observed => "observed",
                                CapabilityProvenanceConfig::Unknown => "unknown",
                            }
                            .to_owned(),
                        )
                    })
                    .collect(),
                None => worker
                    .capabilities
                    .iter()
                    .map(|capability| (capability.as_str().to_owned(), "declared".to_owned()))
                    .collect(),
            };
            let mut blockers = Vec::new();
            if !worker.available {
                blockers.push(Blocker::Disabled);
            }
            if capabilities.is_empty() {
                blockers.push(Blocker::NoCapabilities);
            } else if capabilities
                .iter()
                .all(|(_, provenance)| provenance == "declared" || provenance == "unknown")
            {
                blockers.push(Blocker::CapabilitiesDeclaredOnly);
            }
            if synthetic_model_identity(worker) {
                blockers.push(Blocker::SyntheticModelIdentity);
            }
            if worker.estimated_cost_microusd.is_none() {
                blockers.push(Blocker::CostUnmeasured);
            }
            let remediation = blockers
                .iter()
                .filter_map(|blocker| blocker.remediation(id))
                .collect();
            WorkerInventoryRow {
                worker_id: id.clone(),
                provider: worker.provider.clone(),
                model: worker.model.clone(),
                runtime: worker.runtime.clone(),
                enabled: worker.available,
                capabilities,
                cost_microusd: worker.estimated_cost_microusd,
                routable: !blockers.iter().any(Blocker::is_fatal),
                blockers,
                remediation,
            }
        })
        .collect()
}

pub fn workers(config: &Config) -> Result<(), String> {
    let rows = inventory(config);
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({ "workers": rows }))
            .map_err(|error| error.to_string())?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::config::{
        AgentAdapterKind, CapabilityProfileConfig, CapabilityProvenanceConfig,
        RegistryWorkerConfig, RuntimeCapabilityConfig, WorkerCapabilityConfig,
        WorkerRegistryConfig,
    };
    use std::collections::BTreeMap;

    fn worker(model: &str, executable: Option<&str>) -> RegistryWorkerConfig {
        RegistryWorkerConfig {
            adapter: Some(AgentAdapterKind::ClaudeCode),
            provider: "anthropic".into(),
            model: model.into(),
            runtime: None,
            model_artifact: None,
            auth_profile: None,
            capability_profile: None,
            runtime_config: None,
            executable: executable.map(str::to_owned),
            capabilities: vec![WorkerCapabilityConfig::Implementation],
            fresh_process_isolation: true,
            context_tokens: 200_000,
            estimated_cost_microusd: None,
            available: true,
            effort: None,
            permission_mode: None,
            extra_args: Vec::new(),
        }
    }

    fn config_with(
        workers: BTreeMap<String, RegistryWorkerConfig>,
        profiles: BTreeMap<String, CapabilityProfileConfig>,
    ) -> Config {
        let registry = WorkerRegistryConfig {
            workers,
            capability_profiles: profiles,
            ..Default::default()
        };
        Config {
            worker_registry: Some(registry),
            ..Config::default()
        }
    }

    fn profile(provenance: CapabilityProvenanceConfig) -> CapabilityProfileConfig {
        CapabilityProfileConfig {
            capabilities: BTreeMap::from([(RuntimeCapabilityConfig::EditsFiles, provenance)]),
        }
    }

    #[test]
    fn a_cli_label_used_as_a_model_is_reported_synthetic_with_remediation() {
        // FAM-BUG-009: `claude/claude` passed verification, enablement,
        // routing, and preflight, then failed every attempt of a wave.
        let mut workers = BTreeMap::new();
        workers.insert("claude-cli".to_string(), worker("claude", Some("claude")));
        let rows = inventory(&config_with(workers, BTreeMap::new()));
        assert!(rows[0].blockers.contains(&Blocker::SyntheticModelIdentity));
        assert!(!rows[0].routable);
        assert!(
            rows[0]
                .remediation
                .iter()
                .any(|r| r.contains("not a model identity")),
            "{:?}",
            rows[0].remediation
        );
    }

    #[test]
    fn declared_only_capability_is_not_routable_and_probing_is_the_remedy() {
        // FAM-FRICTION-003: declared capability is a claim, not evidence.
        let mut declared = worker("claude-sonnet-4-5", Some("claude"));
        declared.capability_profile = Some("declared-only".into());
        let mut workers = BTreeMap::new();
        workers.insert("declared".to_string(), declared);
        let profiles = BTreeMap::from([(
            "declared-only".to_string(),
            profile(CapabilityProvenanceConfig::Declared),
        )]);
        let rows = inventory(&config_with(workers, profiles));
        assert!(rows[0]
            .blockers
            .contains(&Blocker::CapabilitiesDeclaredOnly));
        assert!(!rows[0].routable);
        assert!(rows[0]
            .remediation
            .iter()
            .any(|r| r.contains("familiar-ai preflight")));
    }

    #[test]
    fn a_probed_enabled_worker_is_routable_and_unmeasured_cost_is_not_fatal() {
        let mut probed = worker("claude-sonnet-4-5", Some("claude"));
        probed.capability_profile = Some("probed".into());
        let mut workers = BTreeMap::new();
        workers.insert("probed".to_string(), probed);
        let profiles = BTreeMap::from([(
            "probed".to_string(),
            profile(CapabilityProvenanceConfig::Probed),
        )]);
        let rows = inventory(&config_with(workers, profiles));
        assert!(rows[0].routable, "{:?}", rows[0].blockers);
        // Unmeasured cost is reported (FAM-BUG-007) but does not block: it
        // means cost cannot RANK this worker, not that it cannot run.
        assert!(rows[0].blockers.contains(&Blocker::CostUnmeasured));
        assert_eq!(rows[0].cost_microusd, None);
    }
}
