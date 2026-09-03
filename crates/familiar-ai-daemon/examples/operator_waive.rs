//! Operator tool: attach a durable human waiver to one open blocking review
//! finding, through the same storage API completion-evidence validates.
//! waive_finding has no CLI surface yet; this is the interim.
//!
//! Usage:
//!   cargo run -q -p familiar-ai-daemon --no-default-features \
//!     --example operator_waive -- <database> <cycle_id> <finding_id> <actor> <reason>

use familiar_ai_storage::{Database, ReviewRepository};

const ACTION: &str = "record a waiver";

fn main() {
    // FAM-BUG-048: this writes durable state directly, bypassing the claim
    // the drive respects. Refuse while a driver owns the control plane.
    let paths = familiar_ai_core::AppPaths::resolve().expect("resolve app paths");
    if let Err(refusal) =
        familiar_ai_daemon::worker_lock::refuse_while_driver_owns(&paths.runtime_dir, ACTION)
    {
        eprintln!("{refusal}");
        std::process::exit(2);
    }

    let mut args = std::env::args().skip(1);
    let (Some(database), Some(cycle_id), Some(finding_id), Some(actor), Some(reason)) = (
        args.next(),
        args.next(),
        args.next(),
        args.next(),
        args.next(),
    ) else {
        eprintln!("usage: operator_waive <database> <cycle_id> <finding_id> <actor> <reason>");
        std::process::exit(2);
    };
    let db = Database::open(std::path::Path::new(&database)).expect("open database");
    let waiver = ReviewRepository::new(db.conn())
        .waive_finding(&cycle_id, &finding_id, &actor, &reason)
        .expect("waive finding");
    println!(
        "waived {} by {} at {}",
        waiver.finding_id, waiver.actor, waiver.created_at
    );
}
