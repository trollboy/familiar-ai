//! Operator tool: attach a durable human waiver to one open blocking review
//! finding, through the same storage API completion-evidence validates.
//! waive_finding has no CLI surface yet; this is the interim.
//!
//! Usage:
//!   cargo run -q -p familiar-ai-daemon --no-default-features \
//!     --example operator_waive -- <database> <cycle_id> <finding_id> <actor> <reason>

use familiar_ai_storage::{Database, ReviewRepository};

fn main() {
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
