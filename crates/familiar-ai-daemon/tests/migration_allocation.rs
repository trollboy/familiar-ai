//! Migration numbers are an allocation, and allocations drift.
//!
//! This has already cost the project twice. PRD-079 minted a migration that
//! collided with 052 during wave 3 and had to be repaired mid-flight, with a
//! collision ledger written into `running_bugs.md`. Then `073_model_residency`
//! was filed under a name nine ahead of the version it actually declares (64),
//! so the next author reading `ls` picked 74 and left a nine-version hole.
//!
//! Both failures are the same shape: the number in the filename and the number
//! the code applies are allowed to disagree, and a PRD can declare a number
//! that is already spoken for without anything noticing until a worker is
//! halfway through implementing it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Every `include_str!("../migrations/NNN_name.sql")` paired with the
/// `version:` that precedes it, read from the migration table itself.
fn declared_migrations() -> Vec<(i64, String)> {
    let source = fs::read_to_string(repo_root().join("crates/familiar-ai-storage/src/migrate.rs"))
        .expect("the migration table must be readable");
    let mut out = Vec::new();
    let mut pending: Option<i64> = None;
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("version: ") {
            if let Ok(value) = rest.trim_end_matches(',').parse::<i64>() {
                pending = Some(value);
            }
        }
        if let Some(start) = trimmed.find("../migrations/") {
            if let Some(version) = pending.take() {
                let file = trimmed[start + "../migrations/".len()..]
                    .split('"')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                out.push((version, file));
            }
        }
    }
    assert!(
        !out.is_empty(),
        "no migrations parsed — the table's shape changed"
    );
    out
}

#[test]
fn every_migrations_filename_matches_the_version_it_declares() {
    let mut mismatches = Vec::new();
    for (version, file) in declared_migrations() {
        let prefix = file
            .split('_')
            .next()
            .and_then(|p| p.parse::<i64>().ok())
            .unwrap_or_else(|| panic!("{file} must start with a numeric prefix"));
        if prefix != version {
            mismatches.push(format!("{file} declares version {version}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "a migration's filename and its applied version must agree, or the next \
         author reads the wrong next number from `ls`:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn migration_versions_are_unique_and_ascending() {
    let declared = declared_migrations();
    let mut seen: BTreeMap<i64, String> = BTreeMap::new();
    let mut previous = 0i64;
    for (version, file) in &declared {
        if let Some(other) = seen.get(version) {
            panic!("version {version} is claimed by both {other} and {file}");
        }
        assert!(
            *version > previous,
            "{file} (version {version}) is out of order after version {previous} — \
             the runner applies the table in array order, so a version that is \
             numerically earlier but positioned later runs in a different order on \
             a fresh database than on an upgraded one"
        );
        previous = *version;
        seen.insert(*version, file.clone());
    }
}

#[test]
fn no_queued_prd_declares_a_migration_number_that_is_already_taken() {
    // The check that would have caught PRD-093 declaring 063 while
    // 063_local_worker_telemetry.sql already existed.
    // A collision is claiming a number someone else took, not the number you
    // authored: an implemented PRD legitimately declares the migration it
    // created, and that file is applied precisely because the PRD shipped. So
    // compare the whole filename, not just the prefix.
    let existing: BTreeMap<String, String> = declared_migrations()
        .into_iter()
        .map(|(_, file)| (file.split('_').next().unwrap_or_default().to_string(), file))
        .collect();

    let prd_dir = repo_root().join("docs/prds");
    let mut collisions = Vec::new();
    for entry in fs::read_dir(&prd_dir).expect("docs/prds must exist") {
        let path = entry.expect("readable entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        for line in body.lines() {
            let Some(start) = line.find("migrations/") else {
                continue;
            };
            let declared_file: String = line[start + "migrations/".len()..]
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            let declared_number: String = declared_file
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if declared_number.len() != 3 {
                continue;
            }
            if let Some(applied) = existing.get(&declared_number) {
                if applied != &declared_file {
                    collisions.push(format!(
                        "{name} declares migration {declared_file}, but {applied} already holds number {declared_number}"
                    ));
                }
            }
        }
    }
    assert!(
        collisions.is_empty(),
        "a queued PRD may not declare a migration number that already exists — a \
         worker implementing it would clobber or conflict:\n{}",
        collisions.join("\n")
    );
}
