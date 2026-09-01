//! Throwaway: compare durable approval substance hashes with the latest
//! cycle's human-review findings.

use familiar_ai_storage::{Database, OrchestrationRepository};

fn main() {
    let database = std::env::args().nth(1).expect("database path");
    let db = Database::open(std::path::Path::new(&database)).expect("open");
    let approved = OrchestrationRepository::new(db.conn())
        .approved_scope_findings("/home/trollboy/Projects/familiar-ai/.git")
        .expect("approved");
    println!("approved substance hashes: {}", approved.len());
    for hash in &approved {
        println!("  approved {hash}");
    }
    let mut stmt = db
        .conn()
        .prepare("SELECT cycle_id, cycle_json FROM review_cycles ORDER BY rowid DESC LIMIT 2")
        .expect("prepare");
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    for (id, json) in rows {
        let cycle: familiar_ai_review::ReviewCycle = serde_json::from_str(&json).expect("cycle");
        println!("cycle {id}");
        for evaluation in &cycle.scope_evaluations {
            for finding in &evaluation.findings {
                if matches!(
                    finding.decision,
                    familiar_ai_review::ScopeDecision::AmbiguousHumanReview
                ) {
                    let hash = familiar_ai_review::scope_finding_substance_hash(finding);
                    println!(
                        "  live {} {} -> {} (match={})",
                        finding.path,
                        finding.policy_snapshot_hash,
                        hash,
                        approved.contains(&hash)
                    );
                }
            }
        }
    }
}
