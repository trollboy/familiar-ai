//! The window's actions, exercised through the same object the buttons call.
//!
//! The GTK layout is thin and visual, but what the buttons *do* writes the
//! backlog and starts processes, so it is verified here rather than by eye.
#![cfg(feature = "tray")]

use std::sync::{Arc, Mutex};

use familiar_ai_core::control_plane::SchedulingPolicy;
use familiar_ai_core::AppPaths;
use familiar_ai_daemon::control_plane::ControlPlaneService;
use familiar_ai_daemon::tray_data::DaemonDataSource;
use familiar_ai_llm::InferenceRouter;
use familiar_ai_storage::Database;
use familiar_ai_tray::data::{Action, ConfigEdit, DataSource, Query};
use tempfile::TempDir;

struct Harness {
    _tmp: TempDir,
    repo: String,
    config: std::path::PathBuf,
    source: DaemonDataSource,
}

/// Deliberately carries a comment recording *why* a ceiling is what it is —
/// this file is the reasoning, not just the values, and a save that drops it
/// destroys the reason the number was chosen.
const CONFIG_FIXTURE: &str = r#"# Familiar configuration.

[driver]
# Raised from 4 after the overnight run; do not raise further without a warrant.
max_prds_per_session = 6
enabled = true
ratio = 0.5

[review]
allowed_paths = ["docs", "crates"]
max_review_attempts = 3

[repositories."/p/one"]
profile = "strict"
"#;

fn harness() -> Harness {
    let tmp = TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("docs/prds")).unwrap();
    std::fs::write(
        repo_dir.join("docs/prds/PRD-1.md"),
        "# PRD-1\n\nThe body of the PRD.\n",
    )
    .unwrap();

    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let db = Arc::new(Mutex::new(db));
    let control = ControlPlaneService::new(db.clone(), SchedulingPolicy::default(), 1);
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let router = Arc::new(InferenceRouter::new(&Default::default()));
    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        CONFIG_FIXTURE,
    )
    .unwrap();
    let paths = AppPaths {
        config_dir: config_dir.clone(),
        data_dir: tmp.path().join("data"),
        state_dir: tmp.path().join("state"),
        runtime_dir: tmp.path().join("runtime"),
        log_dir: tmp.path().join("log"),
        socket_path: tmp.path().join("run.sock"),
        pid_path: tmp.path().join("run.pid"),
    };
    let repo = repo_dir.to_string_lossy().into_owned();
    Harness {
        source: DaemonDataSource::new(db, router, runtime, control, paths),
        repo,
        config: config_dir.join("config.toml"),
        _tmp: tmp,
    }
}

#[test]
fn starting_a_prd_registers_the_project_and_queues_a_run() {
    let h = harness();

    // Nothing is registered until something is started, and the window must be
    // able to say so rather than claiming the project is active.
    let before = h
        .source
        .query(Query::ProjectState {
            repo: h.repo.clone(),
        })
        .unwrap();
    assert!(before["state"].is_null());

    let ack = h
        .source
        .act(Action::StartPrd {
            repo: h.repo.clone(),
            prd_path: "docs/prds/PRD-1.md".into(),
        })
        .expect("start should queue an execution");
    assert!(ack["execution_id"].as_str().unwrap().starts_with("exec-"));
    assert_eq!(ack["duplicate"], false);

    let after = h
        .source
        .query(Query::ProjectState {
            repo: h.repo.clone(),
        })
        .unwrap();
    assert_eq!(after["state"], "active");

    let runs = h
        .source
        .query(Query::Executions {
            repo: h.repo.clone(),
            limit: 10,
        })
        .unwrap();
    let items = runs["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["state"], "queued");
    // The command is the one the operator would have typed.
    let command = items[0]["command_json"].as_str().unwrap();
    assert!(command.contains("familiar-ai"));
    assert!(command.contains("docs/prds/PRD-1.md"));
}

#[test]
fn pausing_and_resuming_moves_the_project_between_states() {
    let h = harness();
    h.source
        .act(Action::StartPrd {
            repo: h.repo.clone(),
            prd_path: "docs/prds/PRD-1.md".into(),
        })
        .unwrap();

    h.source
        .act(Action::SetProjectPaused {
            repo: h.repo.clone(),
            paused: true,
        })
        .unwrap();
    assert_eq!(
        h.source
            .query(Query::ProjectState {
                repo: h.repo.clone()
            })
            .unwrap()["state"],
        "paused"
    );

    // Starting something else must not quietly resume a paused project: the
    // registration that Start performs has to leave the state alone.
    h.source
        .act(Action::StartPrd {
            repo: h.repo.clone(),
            prd_path: "docs/prds/PRD-1.md".into(),
        })
        .unwrap();
    assert_eq!(
        h.source
            .query(Query::ProjectState {
                repo: h.repo.clone()
            })
            .unwrap()["state"],
        "paused",
        "starting a run must not unpause the project"
    );

    h.source
        .act(Action::SetProjectPaused {
            repo: h.repo.clone(),
            paused: false,
        })
        .unwrap();
    assert_eq!(
        h.source
            .query(Query::ProjectState {
                repo: h.repo.clone()
            })
            .unwrap()["state"],
        "active"
    );
}

/// The backlog refuses unattributed recovery, and the window must hit that
/// refusal before it writes anything rather than after.
#[test]
fn recovery_without_attribution_is_refused() {
    let h = harness();
    for (actor, reason) in [("", "a reason"), ("human:me", ""), ("", "")] {
        let error = h
            .source
            .act(Action::ReleasePrd {
                repo: h.repo.clone(),
                prd_path: "docs/prds/PRD-1.md".into(),
                actor: actor.into(),
                reason: reason.into(),
            })
            .expect_err("empty attribution must be refused");
        assert!(!error.is_empty());
    }
}

#[test]
fn a_prd_can_be_read_and_paths_outside_the_repository_cannot() {
    let h = harness();
    let text = h
        .source
        .query(Query::PrdText {
            repo: h.repo.clone(),
            prd_path: "docs/prds/PRD-1.md".into(),
        })
        .unwrap();
    assert!(text["text"].as_str().unwrap().contains("The body of the PRD"));

    // Containment: a path that climbs out of the repository is refused rather
    // than read.
    let escaped = h.source.query(Query::PrdText {
        repo: h.repo.clone(),
        prd_path: "../../../../etc/passwd".into(),
    });
    assert!(escaped.is_err(), "must not read outside the repository");
}

#[test]
fn stopping_an_unknown_execution_reports_rather_than_pretending() {
    let h = harness();
    h.source
        .act(Action::StartPrd {
            repo: h.repo.clone(),
            prd_path: "docs/prds/PRD-1.md".into(),
        })
        .unwrap();
    let result = h
        .source
        .act(Action::CancelExecution {
            repo: h.repo.clone(),
            execution_id: "exec-does-not-exist".into(),
        })
        .unwrap();
    assert_eq!(result["stopped"], false);
}

// -------------------------------------------------------------- config form

#[test]
fn the_config_document_is_offered_as_it_actually_is() {
    let h = harness();
    let doc = h.source.query(Query::ConfigDocument).unwrap();
    assert!(doc["path"].as_str().unwrap().ends_with("config.toml"));
    assert_eq!(doc["document"]["driver"]["max_prds_per_session"], 6);
    assert_eq!(doc["document"]["driver"]["enabled"], true);
    assert_eq!(doc["document"]["review"]["allowed_paths"][1], "crates");
}

#[test]
fn saving_changes_values_and_keeps_the_comments() {
    let h = harness();
    h.source
        .act(Action::SaveConfig {
            edits: vec![
                ConfigEdit {
                    path: vec!["driver".into(), "max_prds_per_session".into()],
                    value: "9".into(),
                },
                ConfigEdit {
                    path: vec!["driver".into(), "enabled".into()],
                    value: "false".into(),
                },
                ConfigEdit {
                    path: vec!["review".into(), "allowed_paths".into()],
                    value: "docs, crates, tests".into(),
                },
            ],
        })
        .unwrap();

    let written = std::fs::read_to_string(&h.config).unwrap();
    assert!(written.contains("max_prds_per_session = 9"));
    assert!(written.contains("enabled = false"));
    assert!(written.contains(r#""tests""#));
    // The reasoning survives the round trip.
    assert!(
        written.contains("do not raise further without a warrant"),
        "comments must survive a save:\n{written}"
    );
    assert!(written.contains("# Familiar configuration."));
    // Types are preserved, not stringified.
    assert!(!written.contains(r#"max_prds_per_session = "9""#));
}

#[test]
fn a_value_of_the_wrong_type_is_refused_and_nothing_is_written() {
    let h = harness();
    let before = std::fs::read_to_string(&h.config).unwrap();
    let error = h
        .source
        .act(Action::SaveConfig {
            edits: vec![ConfigEdit {
                path: vec!["driver".into(), "max_prds_per_session".into()],
                value: "lots".into(),
            }],
        })
        .expect_err("a non-number for an integer setting must be refused");
    assert!(error.contains("max_prds_per_session"), "names the setting: {error}");
    assert!(error.contains("whole number"), "says what was expected: {error}");
    assert_eq!(std::fs::read_to_string(&h.config).unwrap(), before);
}

/// One bad edit in a batch must not leave the others applied.
#[test]
fn a_rejected_edit_abandons_the_whole_save() {
    let h = harness();
    let before = std::fs::read_to_string(&h.config).unwrap();
    h.source
        .act(Action::SaveConfig {
            edits: vec![
                ConfigEdit {
                    path: vec!["driver".into(), "enabled".into()],
                    value: "false".into(),
                },
                ConfigEdit {
                    path: vec!["driver".into(), "ratio".into()],
                    value: "not a number".into(),
                },
            ],
        })
        .expect_err("the batch must fail");
    assert_eq!(
        std::fs::read_to_string(&h.config).unwrap(),
        before,
        "the first edit must not have landed"
    );
}

#[test]
fn an_unknown_setting_is_refused_by_name() {
    let h = harness();
    let error = h
        .source
        .act(Action::SaveConfig {
            edits: vec![ConfigEdit {
                path: vec!["driver".into(), "no_such_setting".into()],
                value: "1".into(),
            }],
        })
        .expect_err("unknown settings must be refused");
    assert!(error.contains("driver.no_such_setting"), "{error}");
}

#[test]
fn a_save_leaves_the_previous_file_recoverable() {
    let h = harness();
    let before = std::fs::read_to_string(&h.config).unwrap();
    let result = h
        .source
        .act(Action::SaveConfig {
            edits: vec![ConfigEdit {
                path: vec!["driver".into(), "max_prds_per_session".into()],
                value: "1".into(),
            }],
        })
        .unwrap();
    let backup = result["backup"].as_str().unwrap();
    assert_eq!(std::fs::read_to_string(backup).unwrap(), before);
    // The operator is told that a restart is needed, rather than assuming the
    // running daemon picked the change up.
    assert!(result["note"].as_str().unwrap().contains("restart"));
}

/// Overriding a setting a project currently inherits means creating it, and
/// the type has to come from the global setting it shadows.
#[test]
fn a_project_can_override_a_setting_it_currently_inherits() {
    let h = harness();
    h.source
        .act(Action::SaveConfig {
            edits: vec![ConfigEdit {
                path: vec![
                    "repositories".into(),
                    "/p/one".into(),
                    "review".into(),
                    "max_review_attempts".into(),
                ],
                value: "7".into(),
            }],
        })
        .unwrap();

    let written = std::fs::read_to_string(&h.config).unwrap();
    let parsed: toml::Value = toml::from_str(&written).unwrap();
    assert_eq!(
        parsed["repositories"]["/p/one"]["review"]["max_review_attempts"]
            .as_integer()
            .unwrap(),
        7,
        "override written as an integer, not a string:\n{written}"
    );
    // The global value is untouched: an override is not an edit of the default.
    assert_eq!(parsed["review"]["max_review_attempts"].as_integer().unwrap(), 3);
    // And the project's existing settings survive.
    assert_eq!(parsed["repositories"]["/p/one"]["profile"].as_str().unwrap(), "strict");
    assert!(written.contains("do not raise further without a warrant"));
}

#[test]
fn a_project_override_of_the_wrong_type_is_refused() {
    let h = harness();
    let before = std::fs::read_to_string(&h.config).unwrap();
    let error = h
        .source
        .act(Action::SaveConfig {
            edits: vec![ConfigEdit {
                path: vec![
                    "repositories".into(),
                    "/p/one".into(),
                    "review".into(),
                    "max_review_attempts".into(),
                ],
                value: "many".into(),
            }],
        })
        .expect_err("the global type is the contract");
    assert!(error.contains("whole number"), "{error}");
    assert_eq!(std::fs::read_to_string(&h.config).unwrap(), before);
}

/// A typo under a project must not silently become a new setting.
#[test]
fn a_project_override_of_an_unknown_setting_is_refused() {
    let h = harness();
    let error = h
        .source
        .act(Action::SaveConfig {
            edits: vec![ConfigEdit {
                path: vec![
                    "repositories".into(),
                    "/p/one".into(),
                    "review".into(),
                    "no_such_setting".into(),
                ],
                value: "1".into(),
            }],
        })
        .expect_err("unknown settings must be refused even under a project");
    assert!(error.contains("no_such_setting"), "{error}");
}
