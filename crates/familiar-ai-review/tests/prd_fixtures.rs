//! Pin the Expected Files parse result of every repository PRD fixture.
//!
//! The parse outcome of each PRD is part of the review-scope contract: a PRD
//! that pins as `Err` cannot be executed with enabled review until its
//! Expected Files section is amended (and this pin updated with it).

use std::fs;
use std::path::PathBuf;

use familiar_ai_review::{parse_expected_files, ExpectedFilesError};

fn prd_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/prds")
}

fn parse(name: &str) -> Result<Vec<String>, ExpectedFilesError> {
    let active = prd_dir().join(name);
    let path = if active.exists() {
        active
    } else {
        prd_dir().join("done").join(name)
    };
    let content = fs::read_to_string(path).expect("PRD fixture readable");
    parse_expected_files(&content)
        .map(|entries| entries.into_iter().map(|entry| entry.normalized).collect())
}

#[test]
fn prd_013_pins_as_a_valid_contract() {
    assert_eq!(
        parse("PRD-013.md").unwrap(),
        vec![
            "crates/familiar-review/src/types.rs",
            "crates/familiar-review/src/policy.rs",
            "crates/familiar-review/src/evidence.rs",
            "crates/familiar-review/src/",
            "crates/familiar-review/src/coordinator.rs",
            "crates/familiar-review/src/package.rs",
            "crates/familiar-review/src/lib.rs",
            "crates/familiar-review/tests/",
            "crates/familiar-core/src/config.rs",
            "config/default.toml",
            "crates/familiar-daemon/src/run.rs",
            "crates/familiar-daemon/tests/cli_run.rs",
            "crates/familiar-storage/src/repos/review.rs",
            "crates/familiar-storage/migrations/",
            "crates/familiar-storage/src/migrate.rs",
            "crates/familiar-storage/tests/integration.rs",
        ]
    );
}

#[test]
fn prd_014_pins_as_a_valid_contract() {
    assert_eq!(
        parse("PRD-014.md").unwrap(),
        vec![
            "crates/familiar-agent/src/claude_code.rs",
            "crates/familiar-agent/src/isolation.rs",
            "crates/familiar-agent/src/codex.rs",
            "crates/familiar-agent/src/agent.rs",
            "crates/familiar-agent/src/lib.rs",
            "crates/familiar-core/src/config.rs",
            "config/default.toml",
            "crates/familiar-daemon/src/run.rs",
            "crates/familiar-daemon/src/bin/familiar.rs",
            "crates/familiar-daemon/tests/",
        ]
    );
}

#[test]
fn prd_015_pins_as_a_valid_rename_contract_with_both_sides() {
    // A rename is authorized only when both sides are declared, so the
    // contract enumerates every old and new crate directory.
    let crates = [
        "agent", "context", "core", "daemon", "llm", "logging", "mcp", "review", "storage",
        "summary", "testutil", "tokens", "tray", "watcher",
    ];
    let mut expected = vec!["Cargo.toml".to_owned(), "Cargo.lock".to_owned()];
    expected.extend(crates.iter().map(|name| format!("crates/familiar-{name}/")));
    expected.extend(
        crates
            .iter()
            .map(|name| format!("crates/familiar-ai-{name}/")),
    );
    expected.extend([
        "Dockerfile".to_owned(),
        "docker-compose.yml".to_owned(),
        "config/".to_owned(),
    ]);
    assert_eq!(parse("PRD-015.md").unwrap(), expected);
}

#[test]
fn prd_016_pins_as_a_valid_contract() {
    assert_eq!(
        parse("PRD-016.md").unwrap(),
        vec![
            "crates/familiar-ai-agent/src/isolation.rs",
            "crates/familiar-ai-agent/src/codex.rs",
            "crates/familiar-ai-agent/src/claude_code.rs",
            "crates/familiar-ai-agent/tests/",
            "Dockerfile",
            "docker-compose.yml",
        ]
    );
}

#[test]
fn prds_017_through_020_pin_as_valid_contracts() {
    assert_eq!(
        parse("PRD-017.md").unwrap(),
        vec![
            "crates/familiar-ai-daemon/src/drive.rs",
            "crates/familiar-ai-daemon/src/lib.rs",
            "crates/familiar-ai-daemon/src/run.rs",
            "crates/familiar-ai-daemon/src/bin/familiar-ai.rs",
            "crates/familiar-ai-core/src/config.rs",
            "config/default.toml",
            "crates/familiar-ai-storage/migrations/",
            "crates/familiar-ai-storage/src/",
            "crates/familiar-ai-storage/tests/",
            "crates/familiar-ai-daemon/tests/",
        ]
    );
    assert_eq!(
        parse("PRD-018.md").unwrap(),
        vec![
            "crates/familiar-ai-daemon/src/report.rs",
            "crates/familiar-ai-daemon/src/lib.rs",
            "crates/familiar-ai-daemon/src/bin/familiar-ai.rs",
            "crates/familiar-ai-storage/src/",
            "crates/familiar-ai-storage/tests/",
            "crates/familiar-ai-daemon/tests/",
        ]
    );
    assert_eq!(
        parse("PRD-019.md").unwrap(),
        vec![
            "crates/familiar-ai-daemon/src/plan.rs",
            "crates/familiar-ai-daemon/src/lib.rs",
            "crates/familiar-ai-daemon/src/bin/familiar-ai.rs",
            "crates/familiar-ai-core/src/config.rs",
            "config/default.toml",
            "crates/familiar-ai-storage/migrations/",
            "crates/familiar-ai-storage/src/",
            "crates/familiar-ai-storage/tests/",
            "crates/familiar-ai-daemon/tests/",
        ]
    );
    assert_eq!(
        parse("PRD-020.md").unwrap(),
        vec![
            "crates/familiar-ai-daemon/src/drive.rs",
            "crates/familiar-ai-daemon/src/worktree.rs",
            "crates/familiar-ai-daemon/src/lib.rs",
            "crates/familiar-ai-daemon/src/report.rs",
            "crates/familiar-ai-daemon/src/bin/familiar-ai.rs",
            "crates/familiar-ai-core/src/config.rs",
            "config/default.toml",
            "crates/familiar-ai-storage/migrations/",
            "crates/familiar-ai-storage/src/",
            "crates/familiar-ai-storage/tests/",
            "crates/familiar-ai-daemon/tests/",
        ]
    );
}

#[test]
fn prd_021_pins_as_a_valid_contract() {
    assert_eq!(
        parse("PRD-021.md").unwrap(),
        vec![
            "crates/familiar-ai-agent/src/isolation.rs",
            "crates/familiar-ai-agent/src/codex.rs",
            "crates/familiar-ai-agent/src/claude_code.rs",
            "crates/familiar-ai-agent/Cargo.toml",
            "Cargo.toml",
            "Cargo.lock",
            "Dockerfile",
            "docker-compose.yml",
        ]
    );
}

#[test]
fn prds_022_and_023_pin_as_valid_contracts() {
    assert_eq!(
        parse("PRD-022.md").unwrap(),
        vec![
            "crates/familiar-ai-core/src/backlog.rs",
            "crates/familiar-ai-storage/src/repos/backlog.rs",
            "crates/familiar-ai-storage/migrations/",
            "crates/familiar-ai-storage/src/migrate.rs",
            "crates/familiar-ai-daemon/src/bin/familiar-ai.rs",
            "crates/familiar-ai-daemon/tests/",
        ]
    );
    assert_eq!(
        parse("PRD-023.md").unwrap(),
        vec![
            "crates/familiar-ai-core/src/backlog.rs",
            "crates/familiar-ai-storage/src/repos/backlog.rs",
            "crates/familiar-ai-daemon/tests/",
            "crates/familiar-ai-review/tests/",
        ]
    );
}

#[test]
fn prd_024_pins_as_a_valid_contract() {
    assert_eq!(
        parse("PRD-024.md").unwrap(),
        vec![
            "crates/familiar-ai-agent/src/agent.rs",
            "crates/familiar-ai-agent/src/claude_code.rs",
            "crates/familiar-ai-agent/src/codex.rs",
            "crates/familiar-ai-agent/tests/",
            "crates/familiar-ai-core/src/config.rs",
            "config/default.toml",
            "crates/familiar-ai-daemon/src/run.rs",
            "crates/familiar-ai-daemon/src/drive.rs",
            "crates/familiar-ai-daemon/src/report.rs",
            "crates/familiar-ai-daemon/tests/",
        ]
    );
}

#[test]
fn prds_025_and_026_pin_as_valid_contracts() {
    assert_eq!(
        parse("PRD-025.md").unwrap(),
        vec![
            "crates/familiar-ai-core/src/backlog.rs",
            "crates/familiar-ai-core/src/bootstrap.rs",
            "crates/familiar-ai-core/src/config.rs",
            "crates/familiar-ai-core/src/lib.rs",
            "crates/familiar-ai-context/src/lib.rs",
            "crates/familiar-ai-storage/migrations/",
            "crates/familiar-ai-storage/src/migrate.rs",
            "crates/familiar-ai-storage/src/repos/backlog.rs",
            "crates/familiar-ai-storage/src/repos/bootstrap.rs",
            "crates/familiar-ai-daemon/src/cli.rs",
            "crates/familiar-ai-daemon/src/run.rs",
            "crates/familiar-ai-daemon/src/drive.rs",
            "crates/familiar-ai-daemon/src/report.rs",
            "crates/familiar-ai-daemon/tests/",
            "config/default.toml",
        ]
    );
    assert_eq!(
        parse("PRD-026.md").unwrap(),
        vec![
            "crates/familiar-ai-core/src/config.rs",
            "crates/familiar-ai-daemon/src/run.rs",
            "crates/familiar-ai-daemon/src/drive.rs",
            "crates/familiar-ai-daemon/src/report.rs",
            "crates/familiar-ai-daemon/tests/",
            "crates/familiar-ai-storage/migrations/",
            "crates/familiar-ai-storage/src/repos/",
            "config/default.toml",
        ]
    );
}

#[test]
fn every_wave_two_prd_parses_deterministically() {
    let mut outcomes = Vec::new();
    let mut names: Vec<_> = [prd_dir(), prd_dir().join("done")]
        .into_iter()
        .flat_map(|directory| fs::read_dir(directory).unwrap())
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            (name.starts_with("PRD-") && name.ends_with(".md")).then_some(name)
        })
        .collect();
    names.sort();
    for name in &names {
        let first = parse(name);
        let second = parse(name);
        assert_eq!(first, second, "non-deterministic parse for {name}");
        outcomes.push((name.clone(), first.is_ok()));
    }
    // Every fixture has a pinned validity. PRDs predating the grammar are not
    // rewritten: some legitimately pin as errors, and 004/005/007 happen to
    // already satisfy the closed grammar.
    let valid: Vec<_> = outcomes
        .iter()
        .filter(|(_, ok)| *ok)
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        valid,
        vec![
            "PRD-004.md",
            "PRD-005.md",
            "PRD-007.md",
            "PRD-013.md",
            "PRD-014.md",
            "PRD-015.md",
            "PRD-016.md",
            "PRD-017.md",
            "PRD-018.md",
            "PRD-019.md",
            "PRD-020.md",
            "PRD-021.md",
            "PRD-022.md",
            "PRD-023.md",
            "PRD-024.md",
            "PRD-025.md",
            "PRD-026.md",
            "PRD-028.md",
            "PRD-029.md",
            "PRD-030.md",
            "PRD-031.md",
            "PRD-032.md",
            "PRD-033.md",
            "PRD-034.md",
            "PRD-035.md",
            "PRD-036.md",
            "PRD-037.md",
            "PRD-038.md",
            "PRD-039.md"
        ]
    );
}
