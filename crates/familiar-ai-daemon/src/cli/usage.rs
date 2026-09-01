//! `familiar-ai usage` — query cached local accounting. With no range,
//! preserves the legacy summary.

use familiar_ai_storage::ExecutionHistoryRepository;

use super::shared::database;

pub fn usage(
    start: Option<&str>,
    end: Option<&str>,
    bucket: &str,
    group_by: Vec<String>,
    filters: Vec<String>,
    dense: bool,
) -> Result<(), String> {
    let db = database()?;
    if let (Some(start), Some(end)) = (start, end) {
        use familiar_ai_storage::{UsageBucket, UsageSeriesRequest};
        let parse = |value: &str| {
            chrono::DateTime::parse_from_rfc3339(value)
                .map(|v| v.with_timezone(&chrono::Utc))
                .map_err(|e| format!("invalid UTC timestamp '{value}': {e}"))
        };
        let bucket = match bucket {
            "hour" => UsageBucket::Hour,
            "day" => UsageBucket::Day,
            "week" => UsageBucket::Week,
            "month" => UsageBucket::Month,
            _ => return Err("bucket must be hour, day, week, or month".into()),
        };
        let mut parsed_filters = std::collections::BTreeMap::new();
        for filter in filters {
            let (key, value) = filter
                .split_once('=')
                .ok_or_else(|| format!("filter must be dimension=value: {filter}"))?;
            parsed_filters.insert(key.into(), value.into());
        }
        let points = familiar_ai_storage::AccountingRepository::new(db.conn())
            .usage_series(&UsageSeriesRequest {
                start: parse(start)?,
                end: parse(end)?,
                bucket,
                group_by,
                filters: parsed_filters,
                dense,
            })
            .map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::to_string_pretty(&points).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    let ledger = familiar_ai_storage::AccountingRepository::new(db.conn())
        .usage()
        .map_err(|e| e.to_string())?;
    println!("Ledger observations: {}", ledger.observations);
    println!(
        "Ledger observations with unknown usage: {}",
        ledger.unknown_observations
    );
    println!(
        "Ledger uncached input tokens: {}",
        ledger.uncached_input_tokens
    );
    println!("Ledger cache-read tokens: {}", ledger.cache_read_tokens);
    println!("Ledger cache-write tokens: {}", ledger.cache_write_tokens);
    println!("Ledger output tokens: {}", ledger.output_tokens);
    println!(
        "Ledger reasoning-output tokens: {}",
        ledger.reasoning_output_tokens
    );
    println!("Ledger local-estimate nanoUSD: {}", ledger.known_nanousd);
    println!(
        "Ledger provenance vendor-reported={} configured-rate={} known-zero={}",
        ledger.vendor_reported_estimates,
        ledger.configured_rate_estimates,
        ledger.known_zero_estimates
    );
    let u = ExecutionHistoryRepository::new(db.conn())
        .usage()
        .map_err(|e| e.to_string())?;
    println!("Executions: {}", u.execution_count);
    println!("Executions with complete usage: {}", u.complete_usage);
    println!("Executions with unknown usage: {}", u.unknown_usage);
    println!("Known input tokens: {}", u.known_input_tokens);
    println!("Known output tokens: {}", u.known_output_tokens);
    println!("Known cached tokens: {}", u.known_cached_tokens);
    if u.cache_measured_input_tokens > 0 {
        println!(
            "Cached input share: {:.2}% ({} measured execution(s))",
            u.known_cached_tokens as f64 * 100.0 / u.cache_measured_input_tokens as f64,
            u.cache_measured_executions
        );
    } else {
        println!("Cached input share: — (no measured input/cache pairs)");
    }
    println!(
        "Cache-unmeasured executions: {}",
        u.cache_unmeasured_executions
    );
    println!(
        "Known cache savings: {} micro-USD ({} execution(s), persisted execution-history pricing)",
        u.known_cache_savings_microusd, u.cache_savings_priced_executions
    );
    println!(
        "Cache-savings attempts without pricing provenance: {}",
        u.cache_savings_unpriced_executions
    );
    println!("Known total tokens: {}", u.known_total_tokens);
    println!("Executions with known cost: {}", u.known_cost_executions);
    println!(
        "Executions with unknown cost: {}",
        u.unknown_cost_executions
    );
    println!(
        "Known estimated cost: {} micro-USD (${:.6})",
        u.known_cost_microusd,
        u.known_cost_microusd as f64 / 1_000_000.0
    );
    Ok(())
}
