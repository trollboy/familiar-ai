//! PRD-076 behavior-preservation oracle: `config.rs` became a `config/`
//! module tree by pure code motion. This test proves the split changed no
//! behavior by resolving the pinned `config/default.toml` fixture through
//! `Config::load` and asserting the effective configuration is byte-identical
//! to `tests/fixtures/prd076_pinned_effective_config.json`, which was
//! captured from the pre-split monolithic `config.rs` at the commit this PRD
//! branched from (1a968ed).

use std::path::{Path, PathBuf};

use familiar_ai_core::config::Config;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn pinned_fixture_resolves_to_identical_effective_configuration() {
    let root = repo_root();
    let fixture = root.join("config/default.toml");
    let config = Config::load(Some(&fixture)).expect("pinned fixture must load");
    let actual = serde_json::to_string_pretty(&config).expect("Config must serialize");

    let pinned_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/prd076_pinned_effective_config.json");
    let expected = std::fs::read_to_string(&pinned_path)
        .expect("pinned effective-config fixture must be readable")
        .trim_end()
        .to_string();

    assert_eq!(
        actual, expected,
        "the config/ module split changed the effective configuration for the pinned fixture; \
         the split must be pure code motion with no behavior change"
    );
}
