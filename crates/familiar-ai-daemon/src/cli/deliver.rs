//! `familiar-ai deliver` — publish, check, merge, deploy to staging, and
//! smoke-test one reviewed worktree under the configured finite delivery
//! policy.

pub fn deliver_command(ownership_record: &std::path::Path, to: Option<&str>) -> Result<(), String> {
    match crate::delivery::execute_configured(ownership_record, to)? {
        crate::delivery::ConfiguredDeliveryOutcome::Environment {
            session_id,
            prd_id,
            role,
            target,
            revision,
        } => println!(
            "delivery_session={session_id} prd={prd_id} role={role} target={target} revision={revision} smoke=passed"
        ),
        crate::delivery::ConfiguredDeliveryOutcome::Standard(result) => println!(
            "delivery_session={} prd={} phase={} change_request={}",
            result.session_id,
            result.prd_id,
            result.phase,
            result
                .change_request
                .map(|change| change.display.unwrap_or(change.id))
                .unwrap_or_else(|| "none".into())
        ),
    }
    Ok(())
}
