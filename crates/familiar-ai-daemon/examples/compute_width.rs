//! Compute the scheduler's own achievable width for the remaining backlog,
//! and for any explicitly named subset. Authored widths have disagreed with
//! the scheduler before (FAM-BUG-010) — this asks the scheduler.
//!
//! Usage:
//!   cargo run -q -p familiar-ai-daemon --no-default-features \
//!     --example compute_width -- [PRD-63 PRD-71 PRD-72 ...]

use familiar_ai_core::{AppPaths, BacklogDiscovery, FilesystemBacklogDiscovery};

fn main() {
    let wanted: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().expect("cwd");
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .expect("repository");
    // Discovery needs the repository's effective config for its layout and
    // risk vocabulary — the same call the drive makes.
    let paths = AppPaths::resolve().expect("app paths");
    let config = familiar_ai_daemon::cli::shared::effective_repository_config(&paths, &cwd)
        .expect("effective config");
    let repository_config = config
        .repository(&repository.worktree)
        .expect("repository config");
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&repository, &repository_config.layout())
        .expect("discover");
    let subset: Vec<_> = discovered
        .into_iter()
        .filter(|prd| {
            prd.location == familiar_ai_core::PrdLocation::Active
                && (wanted.is_empty() || wanted.iter().any(|id| id == &prd.id.to_string()))
        })
        .collect();
    println!(
        "considering: {}",
        subset
            .iter()
            .map(|p| p.id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let report = familiar_ai_daemon::drive::achievable_width(&repository.worktree, &subset)
        .expect("compute width");
    println!(
        "graph_width={} achievable_width={}",
        report.graph_width, report.achievable_width
    );
    if report.conflicts.is_empty() {
        println!("conflicts: none — every pair is scope-disjoint");
    } else {
        for (a, b, reason) in &report.conflicts {
            println!("conflict: {a} <-> {b}: {reason}");
        }
    }
}
