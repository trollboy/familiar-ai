//! Operator tool: rebind a checkpoint to its worktree's current candidate
//! content after a surgical operator edit (e.g. fixing a dangling doc
//! reference that blocks resume's context compilation).
//!
//! Recomputes the candidate snapshot with the same computation freeze and
//! validate use, then updates the durable checkpoint. Prints old and new
//! hashes for the audit trail.
//!
//! Usage:
//!   cargo run -q -p familiar-ai-daemon --no-default-features \
//!     --example operator_rebind -- <database> <repository_key> <prd_id>

use familiar_ai_storage::{CheckpointRepository, Database};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(database), Some(repository_key), Some(prd_id)) =
        (args.next(), args.next(), args.next())
    else {
        eprintln!("usage: operator_rebind <database> <repository_key> <prd_id>");
        std::process::exit(2);
    };
    let db = Database::open(std::path::Path::new(&database)).expect("open database");
    let repository = CheckpointRepository::new(db.conn());
    let mut checkpoint = repository
        .get(&repository_key, &prd_id)
        .expect("read checkpoint")
        .unwrap_or_else(|| panic!("no checkpoint for {repository_key} {prd_id}"));
    println!(
        "checkpoint {} phase={} worktree={}",
        checkpoint.checkpoint_id, checkpoint.phase, checkpoint.worktree_path
    );
    println!("old diff_hash: {}", checkpoint.diff_hash);
    let (evidence, files) = familiar_ai_daemon::resume::candidate_snapshot(
        std::path::Path::new(&checkpoint.worktree_path),
        &checkpoint.base_revision,
    )
    .expect("candidate snapshot");
    checkpoint.diff_hash = familiar_ai_review::content_hash(&evidence);
    checkpoint.changed_files_json =
        serde_json::to_string(&files).expect("serialize changed files");
    println!("new diff_hash: {}", checkpoint.diff_hash);
    println!("manifest files: {}", files.len());
    repository.put(&checkpoint).expect("rebind checkpoint");
    println!("rebound.");
}
