use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use familiar_ai_core::Config;
use rusqlite::Connection;
use tempfile::tempdir;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn split_defaults_resolve_identically_to_legacy_default_toml() {
    let legacy = Config::load(Some(&root().join("config/default.toml"))).unwrap();
    let split = Config::load_defaults_dir(&root().join("config/default.d")).unwrap();
    assert_eq!(
        serde_json::to_value(legacy).unwrap(),
        serde_json::to_value(split).unwrap()
    );
}

#[test]
fn defaults_are_assembled_in_filename_order() {
    let temp = tempdir().unwrap();
    fs::write(
        temp.path().join("20-later.toml"),
        "[logging]\nlevel = 'debug'\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("10-earlier.toml"),
        "[logging]\nlevel = 'warn'\n",
    )
    .unwrap();
    let loaded = Config::load_defaults_dir(temp.path()).unwrap();
    assert_eq!(loaded.logging.level, "debug");
}

#[test]
fn migration_registry_is_directory_derived() {
    let migrate =
        fs::read_to_string(root().join("crates/familiar-ai-storage/src/migrate.rs")).unwrap();
    let build = fs::read_to_string(root().join("crates/familiar-ai-storage/build.rs")).unwrap();
    assert!(migrate.contains("include!(concat!(env!(\"OUT_DIR\")"));
    assert!(!migrate.contains("include_str!(\"../migrations/"));
    assert!(build.contains("fs::read_dir(dir)"));
    assert!(build.contains("migrations.sort_by_key"));
    assert!(build.contains("pair[0].0 == pair[1].0"));
}

#[test]
fn adding_a_fixture_migration_does_not_require_migrate_rs() {
    let before = fs::read(root().join("crates/familiar-ai-storage/src/migrate.rs")).unwrap();
    let temp = tempdir().unwrap();
    let crate_dir = temp.path().join("crate");
    let migrations = crate_dir.join("migrations");
    let output = temp.path().join("out");
    fs::create_dir_all(&migrations).unwrap();
    fs::create_dir_all(&output).unwrap();
    let fixture = migrations.join("999_fixture.sql");
    fs::write(&fixture, "CREATE TABLE fixture(id INTEGER);").unwrap();

    let build_binary = temp.path().join("build-script");
    assert!(Command::new("rustc")
        .arg(root().join("crates/familiar-ai-storage/build.rs"))
        .args(["-o"])
        .arg(&build_binary)
        .status()
        .unwrap()
        .success());
    assert!(Command::new(build_binary)
        .env("CARGO_MANIFEST_DIR", &crate_dir)
        .env("OUT_DIR", &output)
        .status()
        .unwrap()
        .success());
    let generated = fs::read_to_string(output.join("migrations.rs")).unwrap();
    assert!(generated.contains("version: 999"));
    assert!(generated.contains("999_fixture.sql"));

    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&fs::read_to_string(fixture).unwrap())
        .unwrap();
    let applied: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='fixture'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(applied, 1);
    assert_eq!(
        before,
        fs::read(root().join("crates/familiar-ai-storage/src/migrate.rs")).unwrap()
    );
}
