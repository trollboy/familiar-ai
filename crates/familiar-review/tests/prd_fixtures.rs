//! Pin the Expected Files parse result of every repository PRD fixture.
//!
//! The parse outcome of each PRD is part of the review-scope contract: a PRD
//! that pins as `Err` cannot be executed with enabled review until its
//! Expected Files section is amended (and this pin updated with it).

use std::fs;
use std::path::PathBuf;

use familiar_review::{parse_expected_files, ExpectedFilesError};

fn prd_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/prds")
}

fn parse(name: &str) -> Result<Vec<String>, ExpectedFilesError> {
    let content = fs::read_to_string(prd_dir().join(name)).expect("PRD fixture readable");
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
fn prd_015_pins_as_a_validation_error_until_amended() {
    // Its Expected Files section uses a non-terminal glob (`crates/familiar-*`),
    // which the closed grammar rejects; PRD-015 must be amended before it can
    // execute under enabled review.
    let error = parse("PRD-015.md").unwrap_err();
    assert!(
        matches!(error, ExpectedFilesError::UnsupportedExpression { .. }),
        "unexpected PRD-015 parse outcome: {error:?}"
    );
}

#[test]
fn every_wave_two_prd_parses_deterministically() {
    let mut outcomes = Vec::new();
    let mut names: Vec<_> = fs::read_dir(prd_dir())
        .unwrap()
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
            "PRD-014.md"
        ]
    );
}
