use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricScope {
    pub repository_key: String,
    pub hosts: Vec<String>,
    pub window_start: String,
    pub window_end: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowMetrics {
    pub scope: MetricScope,
    pub accepted_prds: u64,
    pub known_cost_microusd: u64,
    pub unknown_cost_attempts: u64,
    pub human_touches: u64,
    pub cost_per_accepted_prd_microusd: Option<f64>,
    pub human_touches_per_accepted_prd: Option<f64>,
}

impl WindowMetrics {
    pub fn new(scope: MetricScope, accepted: u64, cost: u64, unknown: u64, touches: u64) -> Self {
        Self {
            scope,
            accepted_prds: accepted,
            known_cost_microusd: cost,
            unknown_cost_attempts: unknown,
            human_touches: touches,
            cost_per_accepted_prd_microusd: (accepted > 0).then(|| cost as f64 / accepted as f64),
            human_touches_per_accepted_prd: (accepted > 0)
                .then(|| touches as f64 / accepted as f64),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricTrend {
    pub current: WindowMetrics,
    pub previous: WindowMetrics,
    pub cost_per_accepted_prd_change_microusd: Option<f64>,
    pub human_touches_per_accepted_prd_change: Option<f64>,
}

impl MetricTrend {
    pub fn new(current: WindowMetrics, previous: WindowMetrics) -> Result<Self, String> {
        if current.scope.repository_key != previous.scope.repository_key {
            return Err("metric trend cannot compare different repository scopes".into());
        }
        Ok(Self {
            cost_per_accepted_prd_change_microusd: difference(
                current.cost_per_accepted_prd_microusd,
                previous.cost_per_accepted_prd_microusd,
            ),
            human_touches_per_accepted_prd_change: difference(
                current.human_touches_per_accepted_prd,
                previous.human_touches_per_accepted_prd,
            ),
            current,
            previous,
        })
    }
}

fn difference(current: Option<f64>, previous: Option<f64>) -> Option<f64> {
    current
        .zip(previous)
        .map(|(current, previous)| current - previous)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncomputableScope {
    pub missing_hosts: Vec<String>,
}

pub fn require_hosts(
    required: &[String],
    contributing: &[String],
) -> Result<(), UncomputableScope> {
    let mut missing: Vec<_> = required
        .iter()
        .filter(|host| !contributing.contains(host))
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(UncomputableScope {
            missing_hosts: missing,
        })
    }
}

pub fn combine_host_windows(
    repository_key: &str,
    required_hosts: &[String],
    windows: &[WindowMetrics],
) -> Result<WindowMetrics, String> {
    let contributing: Vec<String> = windows
        .iter()
        .flat_map(|window| window.scope.hosts.clone())
        .collect();
    require_hosts(required_hosts, &contributing).map_err(|missing| {
        format!(
            "project-wide metric is uncomputable; missing host ledger(s): {}",
            missing.missing_hosts.join(", ")
        )
    })?;
    if windows
        .iter()
        .any(|window| window.scope.repository_key != repository_key)
    {
        return Err("project-wide metric cannot mix repository scopes".into());
    }
    let first = windows
        .first()
        .ok_or("project-wide metric has no contributing ledgers")?;
    if windows.iter().any(|window| {
        window.scope.window_start != first.scope.window_start
            || window.scope.window_end != first.scope.window_end
    }) {
        return Err("project-wide metric cannot mix bounded windows".into());
    }
    Ok(WindowMetrics::new(
        MetricScope {
            repository_key: repository_key.into(),
            hosts: required_hosts.to_vec(),
            window_start: first.scope.window_start.clone(),
            window_end: first.scope.window_end.clone(),
        },
        windows.iter().map(|value| value.accepted_prds).sum(),
        windows.iter().map(|value| value.known_cost_microusd).sum(),
        windows
            .iter()
            .map(|value| value.unknown_cost_attempts)
            .sum(),
        windows.iter().map(|value| value.human_touches).sum(),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryClaim {
    pub repository_key: String,
    pub hosts: Vec<String>,
    pub window_start: String,
    pub window_end: String,
    pub accepted_prds: u64,
    pub unattended_prds: u64,
    pub measured_cost_executions: u64,
    pub total_cost_executions: u64,
}

/// Claims are embedded as one JSON object after `familiar-delivery-claim:`.
pub fn claims(document: &str) -> Result<Vec<DeliveryClaim>, String> {
    document
        .lines()
        .filter_map(|line| {
            line.split_once("familiar-delivery-claim:")
                .map(|(_, json)| json.trim())
        })
        .map(|json| serde_json::from_str(json).map_err(|e| format!("invalid delivery claim: {e}")))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionOutcomeClosure {
    pub bug: String,
    pub query: Option<String>,
    pub recorded_result: Option<String>,
}

pub fn validate_execution_outcome_closure(value: &ExecutionOutcomeClosure) -> Result<(), String> {
    if value.query.as_deref().is_none_or(str::is_empty)
        || value.recorded_result.as_deref().is_none_or(str::is_empty)
    {
        Err(format!("{} closure requires the execution-outcome query and recorded result; narrative evidence is insufficient", value.bug))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(repository: &str, host: &str, start: &str, end: &str) -> MetricScope {
        MetricScope {
            repository_key: repository.into(),
            hosts: vec![host.into()],
            window_start: start.into(),
            window_end: end.into(),
        }
    }

    #[test]
    fn repository_scopes_never_mix() {
        let familiar = WindowMetrics::new(
            scope("familiar.git", "linux", "2026-09-01", "2026-10-01"),
            2,
            200,
            0,
            1,
        );
        let spectra = WindowMetrics::new(
            scope("spectra.git", "linux", "2026-09-01", "2026-10-01"),
            40,
            1,
            0,
            0,
        );
        let error = combine_host_windows("familiar.git", &["linux".into()], &[familiar, spectra])
            .unwrap_err();
        assert_eq!(error, "project-wide metric cannot mix repository scopes");
    }

    #[test]
    fn project_claim_refuses_and_names_missing_hosts() {
        let local = WindowMetrics::new(
            scope("familiar.git", "linux", "2026-09-01", "2026-10-01"),
            2,
            200,
            1,
            1,
        );
        let error =
            combine_host_windows("familiar.git", &["linux".into(), "m1-mac".into()], &[local])
                .unwrap_err();
        assert_eq!(
            error,
            "project-wide metric is uncomputable; missing host ledger(s): m1-mac"
        );
    }

    #[test]
    fn two_window_trend_reports_absolute_rates_and_changes() {
        let previous = WindowMetrics::new(
            scope("familiar.git", "linux", "2026-08-01", "2026-09-01"),
            2,
            1_000,
            0,
            6,
        );
        let current = WindowMetrics::new(
            scope("familiar.git", "linux", "2026-09-01", "2026-10-01"),
            4,
            1_200,
            1,
            4,
        );
        let trend = MetricTrend::new(current, previous).unwrap();
        assert_eq!(trend.current.cost_per_accepted_prd_microusd, Some(300.0));
        assert_eq!(trend.cost_per_accepted_prd_change_microusd, Some(-200.0));
        assert_eq!(trend.current.human_touches_per_accepted_prd, Some(1.0));
        assert_eq!(trend.human_touches_per_accepted_prd_change, Some(-2.0));
        assert_eq!(trend.current.unknown_cost_attempts, 1);
    }
}
