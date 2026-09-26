use familiar_ai_core::metrics::north_star::{claims, require_hosts, DeliveryClaim, MetricTrend};
use familiar_ai_storage::{Database, DeliveryLedgerRepository, HostLedgerScope};

#[derive(Debug, Clone)]
pub struct MetricsRequest {
    pub host: String,
    pub repository_key: String,
    pub current_start: String,
    pub current_end: String,
    pub previous_start: String,
    pub previous_end: String,
    pub required_hosts: Vec<String>,
}

pub fn render(db: &Database, request: &MetricsRequest) -> Result<String, String> {
    let available = vec![request.host.clone()];
    require_hosts(&request.required_hosts, &available).map_err(|missing| {
        format!(
            "project-wide metric is uncomputable; missing host ledger(s): {}",
            missing.missing_hosts.join(", ")
        )
    })?;
    let ledger = DeliveryLedgerRepository::new(db.conn());
    let scope = HostLedgerScope {
        host_id: request.host.clone(),
        repository_key: request.repository_key.clone(),
    };
    let current = ledger
        .window_metrics(&scope, &request.current_start, &request.current_end)
        .map_err(|e| e.to_string())?;
    let previous = ledger
        .window_metrics(&scope, &request.previous_start, &request.previous_end)
        .map_err(|e| e.to_string())?;
    let trend = MetricTrend::new(current, previous)?;
    serde_json::to_string_pretty(&trend).map_err(|e| e.to_string())
}

pub fn command(request: MetricsRequest) -> Result<(), String> {
    let db = super::shared::database()?;
    println!("{}", render(&db, &request)?);
    Ok(())
}

pub fn check_document_claims(
    db: &Database,
    local_host: &str,
    source: &str,
) -> Result<usize, String> {
    let claims = claims(source)?;
    for claim in &claims {
        check_claim(db, local_host, claim)?;
    }
    Ok(claims.len())
}

fn check_claim(db: &Database, local_host: &str, claim: &DeliveryClaim) -> Result<(), String> {
    require_hosts(&claim.hosts, &[local_host.to_owned()]).map_err(|missing| {
        format!(
            "delivery claim is uncomputable; missing host ledger(s): {}",
            missing.missing_hosts.join(", ")
        )
    })?;
    let actual = DeliveryLedgerRepository::new(db.conn())
        .claim_facts(
            &claim.repository_key,
            &claim.window_start,
            &claim.window_end,
        )
        .map_err(|e| e.to_string())?;
    let expected = (
        claim.accepted_prds,
        claim.unattended_prds,
        claim.measured_cost_executions,
        claim.total_cost_executions,
    );
    let observed = (
        actual.accepted_prds,
        actual.unattended_prds,
        actual.measured_cost_executions,
        actual.total_cost_executions,
    );
    if expected != observed {
        return Err(format!("stale delivery claim for repository={} hosts={} window={}..{}: claimed accepted/unattended/measured/total={}/{}/{}/{} but ledger={}/{}/{}/{}",claim.repository_key,claim.hosts.join(","),claim.window_start,claim.window_end,expected.0,expected.1,expected.2,expected.3,observed.0,observed.1,observed.2,observed.3));
    }
    Ok(())
}
