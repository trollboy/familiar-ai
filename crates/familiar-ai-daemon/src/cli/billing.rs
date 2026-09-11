//! `familiar-ai billing` / `history` / `usage` — the accounting surface:
//! cached authoritative billing, standalone execution history, and cached
//! local usage/cost accounting.

use familiar_ai_core::Config;
use familiar_ai_storage::{Database, ExecutionHistoryRepository};

use super::database;

#[derive(Debug, clap::Subcommand)]
pub enum BillingCommand {
    /// Cached status only; never contacts a provider.
    Status,
    /// Explicitly contact configured organization billing sources.
    Collect {
        source: Option<String>,
        #[arg(long)]
        month: Option<String>,
    },
}

pub fn billing_command(command: BillingCommand) -> Result<(), String> {
    use chrono::{Datelike, NaiveDate, Utc};
    use familiar_ai_core::config::EndpointProviderKind;
    let context = crate::config_cli::ConfigContext::resolve()?;
    let config = Config::load(Some(&context.config_path)).map_err(|e| e.to_string())?;
    let db = Database::open(&config.database.resolve_path(&context.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    let repo = familiar_ai_storage::BillingRepository::new(db.conn());
    match command {
        BillingCommand::Status => {
            let statuses = repo.statuses().map_err(|e| e.to_string())?;
            if statuses.is_empty() {
                println!("no operator-bound billing sources; local-estimate-only coverage");
                return Ok(());
            }
            for row in statuses {
                let stale = row
                    .last_success
                    .as_deref()
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                    .map(|v| {
                        Utc::now()
                            .signed_duration_since(v.with_timezone(&Utc))
                            .num_hours()
                    })
                    .map(|h| format!("{h}h"))
                    .unwrap_or_else(|| "never".into());
                println!("{} organization=\"{}\" last_success={} staleness={} coverage={}..{} failure={}",row.source_name,row.organization_name,row.last_success.as_deref().unwrap_or("never"),stale,row.window_start.as_deref().unwrap_or("none"),row.window_end.as_deref().unwrap_or("none"),row.last_failure.as_deref().unwrap_or("none"));
            }
            Ok(())
        }
        BillingCommand::Collect { source, month } => {
            let today = Utc::now().date_naive();
            let first = if let Some(month) = month {
                NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")
                    .map_err(|_| "--month must be YYYY-MM".to_string())?
            } else {
                today.with_day(1).unwrap()
            };
            if first > today {
                return Err("cannot collect a future billing month".into());
            }
            let next = if first.month() == 12 {
                NaiveDate::from_ymd_opt(first.year() + 1, 1, 1).unwrap()
            } else {
                NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1).unwrap()
            };
            let end = next.min(today);
            if end <= first {
                return Err("the current daily bucket is not complete; no authoritative window is available yet".into());
            }
            let start = format!("{}T00:00:00Z", first.format("%Y-%m-%d"));
            let end = format!("{}T00:00:00Z", end.format("%Y-%m-%d"));
            let selected = config
                .providers
                .iter()
                .filter(|(name, p)| {
                    p.kind == EndpointProviderKind::Billing
                        && source
                            .as_deref()
                            .map_or(true, |wanted| wanted == name.as_str())
                })
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err(source.map_or_else(
                    || "no operator-bound billing sources".into(),
                    |v| format!("unknown billing source '{v}'"),
                ));
            }
            for (name, provider) in selected {
                let added = crate::billing::collect(name, provider, &start, &end, &repo)?;
                println!("{name}: complete {start}..{end}, {added} new revisions");
            }
            Ok(())
        }
    }
}

pub fn history(limit: u8, verbose: bool) -> Result<(), String> {
    let db = database()?;
    let rows = ExecutionHistoryRepository::new(db.conn())
        .recent(limit)
        .map_err(|e| e.to_string())?;
    if rows.is_empty() {
        println!("No execution history.");
        return Ok(());
    }
    for row in rows {
        let duration = row
            .duration_ms
            .map(|v| format!("{v}ms"))
            .unwrap_or_else(|| "—".into());
        let model = row.model.as_deref().unwrap_or("—");
        let status = match (row.exit_code, row.signal) {
            (Some(code), _) => format!("{} ({code})", row.outcome),
            (None, Some(signal)) => format!("{} (signal {signal})", row.outcome),
            _ if row.outcome == "running" => "running/incomplete".into(),
            _ => row.outcome.clone(),
        };
        println!(
            "{}  {}  {}  {}  {}  {}  {}  {}  {}",
            row.execution_id,
            row.started_at,
            duration,
            row.agent,
            model,
            status,
            row.repository,
            row.worktree,
            row.prd_path
        );
        if verbose {
            for (field, reason) in row.unavailable_fields {
                println!("  {field}: — ({reason})");
            }
        }
    }
    Ok(())
}

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
