//! Operator tool: transition a checkpoint's phase through the audited
//! transition API (writes a sequenced checkpoint event naming the reason).
//!
//! Usage:
//!   cargo run -q -p familiar-ai-daemon --no-default-features \
//!     --example operator_set_phase -- <database> <checkpoint_id> <phase> <detail>

use familiar_ai_storage::{CheckpointRepository, Database};

const ACTION: &str = "transition a checkpoint phase";

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
    let (Some(database), Some(checkpoint_id), Some(phase), Some(detail)) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        eprintln!("usage: operator_set_phase <database> <checkpoint_id> <phase> <detail>");
        std::process::exit(2);
    };
    let db = Database::open(std::path::Path::new(&database)).expect("open database");
    CheckpointRepository::new(db.conn())
        .transition(&checkpoint_id, &phase, &detail)
        .expect("transition");
    println!("{checkpoint_id} -> {phase}");
}
