pub fn deliver_command(ownership_record: &std::path::Path, to: Option<&str>) -> Result<(), String> {
    match familiar_ai_daemon::delivery::execute_configured(ownership_record, to)? {
        familiar_ai_daemon::delivery::ConfiguredDeliveryOutcome::Environment {
            session_id,
            prd_id,
            role,
            target,
            revision,
        } => println!(
            "delivery_session={session_id} prd={prd_id} role={role} target={target} revision={revision} smoke=passed"
        ),
        familiar_ai_daemon::delivery::ConfiguredDeliveryOutcome::Standard(result) => println!(
            "delivery_session={} prd={} phase={} pr={}",
            result.session_id,
            result.prd_id,
            result.phase,
            result
                .pr_number
                .map(|number| number.to_string())
                .unwrap_or_else(|| "unknown".into())
        ),
    }
    Ok(())
}
