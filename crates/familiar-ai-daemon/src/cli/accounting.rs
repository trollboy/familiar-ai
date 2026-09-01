//! `familiar-ai accounting` — reconciliation-aware, source-attributed cost
//! reporting (PRD-053). Every command is read-only, cached-local data; no
//! command in this module ever performs network collection.

use clap::Subcommand;

use super::shared::database;

#[derive(Debug, Subcommand)]
pub enum AccountingCommand {
    /// Month-to-date cost per billing source, from the current-effective
    /// reconciliation projection. Every amount is labeled by authority
    /// (estimated/authoritative/unattributed) and by completeness.
    MonthToDate,
    /// Cost per (PRD, worker) from local estimates, labeled by authority and
    /// completeness — PRD-032 scoring input. `authority=unknown` must never
    /// be treated as free by a caller ranking workers by cost.
    PrdCost,
}

pub fn accounting_command(command: AccountingCommand) -> Result<(), String> {
    let db = database()?;
    let repo = familiar_ai_storage::AccountingRepository::new(db.conn());
    match command {
        AccountingCommand::MonthToDate => {
            let report = repo
                .month_to_date_report(chrono::Utc::now())
                .map_err(|e| e.to_string())?;
            if report.sources.is_empty() {
                println!(
                    "no reconciled billing sources this month; run `familiar-ai billing collect` or `familiar-ai billing reconcile <source>` first"
                );
                return Ok(());
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        AccountingCommand::PrdCost => {
            let scores = repo.accepted_prd_cost().map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&scores).map_err(|e| e.to_string())?
            );
            Ok(())
        }
    }
}
