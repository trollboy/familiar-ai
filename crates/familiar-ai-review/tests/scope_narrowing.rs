//! Pin the narrowed `expected_files` declarations PRD-076 authorized for the
//! pending backlog, and prove the scheduler's own conflict rule now clears
//! the first post-076 round at a width greater than one.
//!
//! Each pinned PRD is contract-v1 (structured front matter is authoritative;
//! see `crates/familiar-ai-review/tests/prd_fixtures.rs` for the legacy
//! `## Expected Files` body-section contract these PRDs do not use), so this
//! file reads the `expected_files:` YAML list directly rather than through
//! `parse_expected_files`, then runs every entry through the same
//! `normalize_scope_path` grammar the scheduler uses.

use std::fs;
use std::path::PathBuf;

use familiar_ai_review::{normalize_scope_path, ExpectedMatchKind};

fn prd_path(name: &str) -> PathBuf {
    let active = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/prds")
        .join(format!("{name}.md"));
    if active.exists() {
        active
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/prds/done")
            .join(format!("{name}.md"))
    }
}

/// Extract the `expected_files:` YAML bullet list from a contract-v1 PRD's
/// front matter without pulling in a YAML parser dependency: the grammar is
/// fixed (`  - <value>` lines immediately following the key).
fn expected_files(name: &str) -> Vec<String> {
    let content = fs::read_to_string(prd_path(name)).expect("PRD fixture readable");
    let mut lines = content.lines();
    for line in &mut lines {
        if line == "expected_files:" {
            break;
        }
    }
    let mut out = Vec::new();
    for line in lines {
        match line.strip_prefix("  - ") {
            Some(rest) => out.push(rest.to_string()),
            None => break,
        }
    }
    out
}

fn normalized(name: &str) -> Vec<(String, ExpectedMatchKind)> {
    expected_files(name)
        .iter()
        .map(|expression| {
            normalize_scope_path(expression)
                .unwrap_or_else(|rule| panic!("PRD-{name}: {expression}: {rule}"))
        })
        .collect()
}

macro_rules! pin {
    ($fn_name:ident, $prd:expr, [$($entry:expr),+ $(,)?]) => {
        #[test]
        fn $fn_name() {
            assert_eq!(
                expected_files($prd),
                vec![$($entry.to_string()),+],
                "PRD-{} expected_files narrowing pin",
                $prd
            );
        }
    };
}

pin!(
    prd_038_pins_narrowed_scope,
    "PRD-038",
    [
        "tests/fixtures/",
        "crates/familiar-ai-daemon/tests/multi_repo_acceptance.rs",
        "crates/familiar-ai-mcp/tests/",
        "docs/acceptance/",
    ]
);

pin!(
    prd_053_pins_narrowed_scope,
    "PRD-053",
    [
        "crates/familiar-ai-core/src/config/execution.rs",
        "crates/familiar-ai-daemon/src/run.rs",
        "crates/familiar-ai-daemon/src/report.rs",
        "crates/familiar-ai-daemon/src/stewardship.rs",
        "crates/familiar-ai-daemon/src/bin/cli/report.rs",
        "crates/familiar-ai-mcp/src/",
        "crates/familiar-ai-storage/migrations/",
        "crates/familiar-ai-storage/src/repos/accounting.rs",
        "crates/familiar-ai-daemon/tests/cost_reconciliation.rs",
        "config/default.toml",
    ]
);

pin!(
    prd_058_pins_narrowed_scope,
    "PRD-058",
    [
        "docs/contracts/agent-loop.md",
        "crates/familiar-ai-agent/src/",
        "crates/familiar-ai-agent/tests/",
        "crates/familiar-ai-llm/src/",
        "crates/familiar-ai-core/src/config/driver.rs",
        "crates/familiar-ai-daemon/src/",
        "crates/familiar-ai-storage/migrations/",
        "crates/familiar-ai-storage/src/repos/checkpoint.rs",
        "crates/familiar-ai-daemon/tests/raw_runtime_loop.rs",
        "config/default.toml",
    ]
);

pin!(
    prd_059_pins_narrowed_scope,
    "PRD-059",
    [
        "docs/contracts/providers/anthropic-api.md",
        "crates/familiar-ai-agent/src/",
        "crates/familiar-ai-agent/tests/",
        "crates/familiar-ai-llm/src/",
        "crates/familiar-ai-core/src/config/providers.rs",
        "crates/familiar-ai-daemon/tests/anthropic_adapter.rs",
        "config/default.toml",
    ]
);

pin!(
    prd_060_pins_narrowed_scope,
    "PRD-060",
    [
        "docs/contracts/providers/openai-api.md",
        "crates/familiar-ai-agent/src/",
        "crates/familiar-ai-agent/tests/",
        "crates/familiar-ai-llm/src/",
        "crates/familiar-ai-core/src/config/providers.rs",
        "crates/familiar-ai-daemon/tests/openai_adapter.rs",
        "config/default.toml",
    ]
);

pin!(
    prd_061_pins_narrowed_scope,
    "PRD-061",
    [
        "docs/contracts/providers/xai-api.md",
        "crates/familiar-ai-agent/src/",
        "crates/familiar-ai-agent/tests/",
        "crates/familiar-ai-llm/src/",
        "crates/familiar-ai-core/src/config/providers.rs",
        "crates/familiar-ai-daemon/tests/xai_adapter.rs",
        "config/default.toml",
    ]
);

pin!(
    prd_063_pins_narrowed_scope,
    "PRD-063",
    [
        "docs/contracts/providers/local-runtime.md",
        "crates/familiar-ai-agent/src/",
        "crates/familiar-ai-agent/tests/",
        "crates/familiar-ai-llm/src/",
        "crates/familiar-ai-core/src/config/registry.rs",
        "crates/familiar-ai-daemon/src/",
        "crates/familiar-ai-storage/migrations/",
        "crates/familiar-ai-storage/src/repos/worker_spec.rs",
        "crates/familiar-ai-daemon/tests/local_worker_runtime.rs",
        "config/default.toml",
    ]
);

pin!(
    prd_071_pins_narrowed_scope,
    "PRD-071",
    [
        "crates/familiar-ai-review/src/",
        "crates/familiar-ai-daemon/src/batch_review.rs",
        "crates/familiar-ai-daemon/src/bin/cli/review.rs",
        "crates/familiar-ai-core/src/config/review.rs",
        "crates/familiar-ai-storage/migrations/",
        "crates/familiar-ai-storage/src/repos/review.rs",
        "crates/familiar-ai-daemon/tests/batch_review.rs",
    ]
);

pin!(
    prd_072_pins_narrowed_scope,
    "PRD-072",
    [
        "docs/contracts/agent-loop.md",
        "crates/familiar-ai-agent/src/",
        "crates/familiar-ai-llm/src/",
        "crates/familiar-ai-core/src/config/driver.rs",
        "crates/familiar-ai-daemon/tests/runtime_token_discipline.rs",
    ]
);

pin!(
    prd_073_pins_narrowed_scope,
    "PRD-073",
    [
        "crates/familiar-ai-llm/src/",
        "crates/familiar-ai-daemon/src/model_residency.rs",
        "crates/familiar-ai-daemon/src/bin/cli/residency.rs",
        "crates/familiar-ai-core/src/config/registry.rs",
        "crates/familiar-ai-storage/migrations/",
        "crates/familiar-ai-storage/src/repos/residency.rs",
        "crates/familiar-ai-daemon/tests/model_residency.rs",
    ]
);

#[test]
fn every_amended_prd_expected_file_parses_under_the_closed_grammar() {
    for name in [
        "PRD-038", "PRD-053", "PRD-058", "PRD-059", "PRD-060", "PRD-061", "PRD-063", "PRD-071",
        "PRD-072", "PRD-073",
    ] {
        for expression in expected_files(name) {
            normalize_scope_path(&expression)
                .unwrap_or_else(|rule| panic!("{name}: {expression}: {rule}"));
        }
    }
}

/// Mirrors `familiar_ai_daemon::drive::scope_entries_overlap` /
/// `scope_overlap`: the migrations directory is exempt (PRD-066 owns
/// migration-number collision safety separately), an exact file conflicts
/// only with an identical exact file, and a directory conflicts with
/// anything nested under it.
fn scope_overlap(
    left: &[(String, ExpectedMatchKind)],
    right: &[(String, ExpectedMatchKind)],
) -> bool {
    const MIGRATIONS: &str = "crates/familiar-ai-storage/migrations/";
    left.iter().any(|(a, a_kind)| {
        if a == MIGRATIONS {
            return false;
        }
        right.iter().any(|(b, b_kind)| {
            if b == MIGRATIONS {
                return false;
            }
            match (a_kind, b_kind) {
                (ExpectedMatchKind::ExactFile, ExpectedMatchKind::ExactFile) => a == b,
                (ExpectedMatchKind::ExactFile, ExpectedMatchKind::Directory) => {
                    a.starts_with(b.as_str())
                }
                (ExpectedMatchKind::Directory, ExpectedMatchKind::ExactFile) => {
                    b.starts_with(a.as_str())
                }
                (ExpectedMatchKind::Directory, ExpectedMatchKind::Directory) => {
                    a.starts_with(b.as_str()) || b.starts_with(a.as_str())
                }
            }
        })
    })
}

/// PRD-076's acceptance criterion: the first post-076 round (wave 5: 038,
/// 053, 058 — see `docs/prds/EXECUTION-PLAN.md`) has a computed achievable
/// width strictly greater than one under the scheduler's own conflict rule.
#[test]
fn wave_five_achieves_width_greater_than_one() {
    let scopes = [
        ("PRD-038", normalized("PRD-038")),
        ("PRD-053", normalized("PRD-053")),
        ("PRD-058", normalized("PRD-058")),
    ];
    let mut best = 0usize;
    for mask in 0u8..(1 << scopes.len()) {
        let picked: Vec<_> = (0..scopes.len())
            .filter(|index| mask & (1 << index) != 0)
            .collect();
        let independent = picked.iter().all(|&i| {
            picked
                .iter()
                .all(|&j| i == j || !scope_overlap(&scopes[i].1, &scopes[j].1))
        });
        if independent {
            best = best.max(picked.len());
        }
    }
    assert!(
        best > 1,
        "wave 5 {:?} achieves only width {best} under the scheduler's conflict rule",
        scopes.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );
    // PRD-038 is disjoint from both 053 and 058; 053 and 058 still share
    // `crates/familiar-ai-daemon/src/` (058's whole-directory declaration,
    // out of PRD-076's scope — see PRD-076's Scope section on PRD-052/054's
    // billing.rs precedent) and `config/default.toml`, so the achievable
    // width is exactly 2, not the graph width of 3.
    assert_eq!(best, 2);
    assert!(
        !scope_overlap(&scopes[0].1, &scopes[1].1),
        "038/053 unexpectedly conflict"
    );
    assert!(
        !scope_overlap(&scopes[0].1, &scopes[2].1),
        "038/058 unexpectedly conflict"
    );
    assert!(
        scope_overlap(&scopes[1].1, &scopes[2].1),
        "053/058 expected to still conflict"
    );
}
