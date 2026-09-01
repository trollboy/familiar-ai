//! `familiar-ai billing` — inspect cached authoritative billing, explicitly
//! collect it, or reconcile it against local estimates (PRD-053).

use clap::Subcommand;
use familiar_ai_core::Config;
use familiar_ai_storage::Database;

#[derive(Debug, Subcommand)]
pub enum BillingCommand {
    /// Cached status only; never contacts a provider.
    Status,
    /// Explicitly contact configured organization billing sources. Runs
    /// reconciliation for the collected window immediately afterward.
    Collect {
        source: Option<String>,
        #[arg(long)]
        month: Option<String>,
    },
    /// Explicitly reconcile a billing source's local estimates against its
    /// current authoritative provider projection. Never contacts a
    /// provider; use `collect` first to refresh authoritative rows.
    Reconcile {
        source: String,
        #[arg(long)]
        month: Option<String>,
    },
}

fn month_window(month: Option<String>) -> Result<(String, String), String> {
    use chrono::{Datelike, NaiveDate, Utc};
    let today = Utc::now().date_naive();
    let first = if let Some(month) = month {
        NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")
            .map_err(|_| "--month must be YYYY-MM".to_string())?
    } else {
        today.with_day(1).unwrap()
    };
    if first > today {
        return Err("cannot address a future billing month".into());
    }
    let next = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1).unwrap()
    };
    let end = next.min(today);
    if end <= first {
        return Err("the current daily bucket is not complete; no window is available yet".into());
    }
    Ok((
        format!("{}T00:00:00Z", first.format("%Y-%m-%d")),
        format!("{}T00:00:00Z", end.format("%Y-%m-%d")),
    ))
}

fn run_reconciliation(
    config: &Config,
    db: &Database,
    source: &str,
    start: &str,
    end: &str,
    invoked_by: &str,
) -> Result<(), String> {
    use chrono::Utc;
    let accounting = familiar_ai_storage::AccountingRepository::new(db.conn());
    let summary = accounting
        .reconcile_window(
            source,
            chrono::DateTime::parse_from_rfc3339(start)
                .map_err(|e| e.to_string())?
                .with_timezone(&Utc),
            chrono::DateTime::parse_from_rfc3339(end)
                .map_err(|e| e.to_string())?
                .with_timezone(&Utc),
            invoked_by,
            i64::try_from(config.reconciliation.tolerance_nanousd).map_err(|e| e.to_string())?,
            i64::from(config.reconciliation.settlement_horizon_days),
            Utc::now(),
            "familiar-ai-cli",
        )
        .map_err(|e| e.to_string())?;
    println!(
        "{source}: reconciled {start}..{end}, {} new rows, {} unchanged",
        summary.rows_appended, summary.rows_unchanged
    );
    for row in &summary.rows {
        println!(
            "  {} {} status={} local={:?} authoritative={:?} variance={:?}",
            row.day_start,
            row.match_key,
            row.status,
            row.local_estimate_nanousd,
            row.authoritative_nanousd,
            row.variance_nanousd
        );
    }
    Ok(())
}

pub fn billing_command(command: BillingCommand) -> Result<(), String> {
    use chrono::Utc;
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
            let (start, end) = month_window(month)?;
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
            let names: Vec<String> = selected.iter().map(|(name, _)| (*name).clone()).collect();
            for (name, provider) in selected {
                let added = crate::billing::collect(name, provider, &start, &end, &repo)?;
                println!("{name}: complete {start}..{end}, {added} new revisions");
            }
            for name in names {
                run_reconciliation(&config, &db, &name, &start, &end, "collect")?;
            }
            Ok(())
        }
        BillingCommand::Reconcile { source, month } => {
            let (start, end) = month_window(month)?;
            run_reconciliation(&config, &db, &source, &start, &end, "explicit")
        }
    }
}
