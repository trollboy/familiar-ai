//! Operator tool: transition a checkpoint's phase through the audited
//! transition API (writes a sequenced checkpoint event naming the reason).
//!
//! Usage:
//!   cargo run -q -p familiar-ai-daemon --no-default-features \
//!     --example operator_set_phase -- <database> <checkpoint_id> <phase> <detail>

use familiar_ai_storage::{CheckpointRepository, Database};

fn main() {
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
