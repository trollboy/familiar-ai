//! The window's actions, exercised through the same object the buttons call.
//!
//! The GTK layout is thin and visual, but what the buttons *do* writes the
//! backlog and starts processes, so it is verified here rather than by eye.

use std::sync::{Arc, Mutex};

use familiar_ai_core::control_plane::SchedulingPolicy;
use familiar_ai_core::AppPaths;
use familiar_ai_daemon::control_plane::ControlPlaneService;
use familiar_ai_daemon::tray_data::DaemonDataSource;
use familiar_ai_llm::InferenceRouter;
use familiar_ai_storage::{Database, DriverRepository};
// The operator contract lives in core; naming it from there keeps these
// tests in the gate, which runs without the `tray` feature (FAM-BUG-097).
use familiar_ai_core::operator_ui::{
    ConfigEdit, OperatorAction as Action, OperatorDataSource as DataSource, OperatorQuery as Query,
};
use tempfile::TempDir;

struct Harness {
    status: Arc<Mutex<familiar_ai_core::AppStatus>>,
    db: Arc<Mutex<Database>>,
    _tmp: TempDir,
    // The source only holds a `Handle`; dropping the runtime here shut it
    // down before any test ran, and every inference save then died with
    // "A Tokio 1.x context was found, but it is being shutdown".
    _runtime: Arc<tokio::runtime::Runtime>,
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

[agents.reviewer]
adapter = "claude-code"
model = "opus"

[repositories."__REPO__"]
# A real profile name: the fixture stands in for a config the daemon loads,
# and a value that fails validation would only ever test the failure path.
profile = "canonical"
"#;

fn harness() -> Harness {
    let tmp = TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("docs/prds")).unwrap();
    std::fs::write(
        repo_dir.join("docs/prds/PRD-1.md"),
        "# PRD-1: Fixture\n\nThe body of the PRD.\n",
    )
    .unwrap();

    // Repository identity is resolved through git, so the fixture has to be a
    // real repository for anything keyed on it to answer. Built through
    // `git_env` so the fixture cannot inherit an ambient GIT_DIR and operate
    // on the repository running the tests.
    assert!(
        familiar_ai_core::git_env::git_command(&repo_dir, &["init", "-q"])
            .status()
            .unwrap()
            .success()
    );

    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let db = Arc::new(Mutex::new(db));
    let control = ControlPlaneService::new(db.clone(), SchedulingPolicy::default(), 1);
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let router = Arc::new(InferenceRouter::new(&Default::default()));
    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    // The fixture names the real temp repository so the daemon's own
    // startup validation, which the save now runs, can load it.
    std::fs::write(
        config_dir.join("config.toml"),
        CONFIG_FIXTURE.replace("__REPO__", &repo_dir.to_string_lossy()),
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
    let status = Arc::new(Mutex::new(familiar_ai_core::AppStatus::new()));
    let reconciler = familiar_ai_daemon::backlog_reconciler::BacklogReconciler::new(
        db.clone(),
        familiar_ai_core::Config::default(),
        std::time::Duration::from_millis(50),
        std::time::Duration::from_secs(3600),
    );
    Harness {
        source: DaemonDataSource::new(
            db.clone(),
            router,
            runtime.handle().clone(),
            control,
            paths,
            status.clone(),
            None,
            reconciler,
        ),
        db,
        status,
        repo,
        config: config_dir.join("config.toml"),
        _tmp: tmp,
        _runtime: runtime,
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
    // FAM-BUG-099: a start is one drive session over exactly this PRD, the
    // path that isolates a worktree and records session, attempt and usage;
    // never `run`, which works in the repository's own checkout.
    let command: serde_json::Value =
        serde_json::from_str(items[0]["command_json"].as_str().unwrap()).unwrap();
    let argv: Vec<&str> = command["argv"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        argv,
        ["familiar-ai", "drive", "--prd", "PRD-1", "--max-prds", "1"],
        "{command}"
    );
    assert_eq!(ack["prd_ids"][0], "PRD-1");
}

/// A wave is one drive session naming every PRD in it, not one run per card.
#[test]
fn launching_a_wave_submits_one_drive_session_over_exactly_those_prds() {
    let h = harness();
    std::fs::write(
        std::path::Path::new(&h.repo).join("docs/prds/PRD-2.md"),
        "# PRD-2: Second fixture\n\nThe second PRD.\n",
    )
    .unwrap();
    let ack = h
        .source
        .act(Action::StartWave {
            repo: h.repo.clone(),
            prd_paths: vec!["docs/prds/PRD-1.md".into(), "docs/prds/PRD-2.md".into()],
        })
        .expect("a wave should queue one execution");
    let runs = h
        .source
        .query(Query::Executions {
            repo: h.repo.clone(),
            limit: 10,
        })
        .unwrap();
    let items = runs["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "one session, not one run per PRD");
    let command: serde_json::Value =
        serde_json::from_str(items[0]["command_json"].as_str().unwrap()).unwrap();
    let argv: Vec<&str> = command["argv"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        argv,
        [
            "familiar-ai",
            "drive",
            "--prd",
            "PRD-1",
            "--prd",
            "PRD-2",
            "--max-prds",
            "2"
        ],
        "{command}"
    );
    assert_eq!(ack["prd_ids"].as_array().unwrap().len(), 2);

    // A path that names no discovered PRD is refused before anything is queued.
    let refused = h
        .source
        .act(Action::StartWave {
            repo: h.repo.clone(),
            prd_paths: vec!["docs/prds/PRD-404.md".into()],
        })
        .unwrap_err();
    assert!(!refused.is_empty());
    let runs = h
        .source
        .query(Query::Executions {
            repo: h.repo.clone(),
            limit: 10,
        })
        .unwrap();
    assert_eq!(runs["items"].as_array().unwrap().len(), 1);
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
    assert!(text["text"]
        .as_str()
        .unwrap()
        .contains("The body of the PRD"));

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
    assert!(
        error.contains("max_prds_per_session"),
        "names the setting: {error}"
    );
    assert!(
        error.contains("whole number"),
        "says what was expected: {error}"
    );
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
                    h.repo.clone(),
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
        parsed["repositories"][h.repo.as_str()]["review"]["max_review_attempts"]
            .as_integer()
            .unwrap(),
        7,
        "override written as an integer, not a string:\n{written}"
    );
    // The global value is untouched: an override is not an edit of the default.
    assert_eq!(
        parsed["review"]["max_review_attempts"]
            .as_integer()
            .unwrap(),
        3
    );
    // And the project's existing settings survive.
    assert_eq!(
        parsed["repositories"][h.repo.as_str()]["profile"]
            .as_str()
            .unwrap(),
        "canonical"
    );
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
                    h.repo.clone(),
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
                    h.repo.clone(),
                    "review".into(),
                    "no_such_setting".into(),
                ],
                value: "1".into(),
            }],
        })
        .expect_err("unknown settings must be refused even under a project");
    assert!(error.contains("no_such_setting"), "{error}");
}

// --------------------------------------------------------- inference set-up

/// The fixture has no `[inference]` table, which is the state of every daemon
/// nobody has configured — and the state in which the tray's inference item
/// matters. The settings still have to be readable, or the form that edits
/// them has nothing to show.
#[test]
fn inference_settings_are_offered_even_when_the_file_has_no_inference_table() {
    let h = harness();
    let document = h.source.query(Query::ConfigDocument).unwrap();
    assert!(
        document["document"].get("inference").is_none(),
        "the fixture is meant to have no inference table"
    );

    let settings = h.source.query(Query::InferenceSettings).unwrap();
    assert_eq!(settings["mode"], "disabled");
    // The defaults come from the schema, so the endpoint and model are
    // present to be edited rather than blank.
    assert!(!settings["builtin_url"].as_str().unwrap().is_empty());
    assert!(!settings["builtin_model"].as_str().unwrap().is_empty());
}

/// The generic config save cannot create the table — it has no existing
/// setting to take the type from — which is why configuring inference has its
/// own action. If this ever starts succeeding, the dedicated path can go.
#[test]
fn the_generic_save_cannot_create_the_inference_table() {
    let h = harness();
    let outcome = h.source.act(Action::SaveConfig {
        edits: vec![ConfigEdit {
            path: vec!["inference".into(), "text".into(), "mode".into()],
            value: "local_only".into(),
        }],
    });
    assert!(outcome.is_err(), "expected a refusal, got {outcome:?}");
}

#[test]
fn saving_inference_creates_the_table_and_marks_it_configured() {
    let h = harness();
    assert!(!h.status.lock().unwrap().local_llm_configured);

    let result = h
        .source
        .act(Action::SaveInferenceConfig {
            mode: "local_only".into(),
            // A port nothing listens on: the save must land and be reported
            // as configured whether or not a backend answers.
            builtin_url: "http://127.0.0.1:1".into(),
            builtin_model: "qwen2.5:3b".into(),
        })
        .expect("the save should land");

    assert_eq!(result["configured"], true);

    // It is in the file...
    let document = h.source.query(Query::ConfigDocument).unwrap();
    assert_eq!(
        document["document"]["inference"]["text"]["mode"],
        "local_only"
    );
    assert_eq!(
        document["document"]["inference"]["text"]["builtin_model"],
        "qwen2.5:3b"
    );

    // ...the rest of the file survived, comments included...
    let text = std::fs::read_to_string(&h.config).unwrap();
    assert!(text.contains("do not raise further without a warrant"));
    assert!(text.contains("max_prds_per_session = 6"));

    // ...the running router picked it up without a restart...
    let status = h.source.query(Query::InferenceStatus).unwrap();
    assert_eq!(status["text_mode"], "localonly");

    // ...and the tray now knows it has something to toggle.
    assert!(h.status.lock().unwrap().local_llm_configured);
}

/// FAM-BUG-087: the desktop's save arrives on a tokio worker (the local
/// transport dispatches operator mutations inline), and the save used to
/// call `Handle::block_on` there, which panics. The other tests call `act`
/// from a plain thread and never saw it.
#[test]
fn saving_inference_from_inside_a_runtime_does_not_panic() {
    let h = harness();
    let worker = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (h, result) = worker.block_on(async move {
        tokio::spawn(async move {
            let result = h.source.act(Action::SaveInferenceConfig {
                mode: "hybrid".into(),
                builtin_url: "http://127.0.0.1:1".into(),
                builtin_model: "qwen2.5:3b".into(),
            });
            (h, result)
        })
        .await
        .expect("the save must not panic on a runtime worker")
    });
    let result = result.expect("the save should land");
    assert_eq!(result["configured"], true);
    assert!(h.status.lock().unwrap().local_llm_configured);
}

/// A one-shot OpenAI-compatible `/v1/models` server on a free local port.
/// Answers every request with the same list until the test ends.
fn serve_models(models: &[&str]) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let body = serde_json::json!({
        "data": models.iter().map(|m| serde_json::json!({"id": m})).collect::<Vec<_>>()
    })
    .to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        }
    });
    url
}

/// FAM-BUG-091: the endpoint answered, so "Healthy" — for a model it does
/// not serve. The save refuses and names what is served; discovery asks the
/// panel's own endpoint; the connection test says the same thing the save
/// would.
#[test]
fn a_model_the_endpoint_does_not_serve_is_refused_and_discovery_lists_what_is() {
    let h = harness();
    let url = serve_models(&["qwen2.5:0.5b", "qwen2.5:7b"]);

    let refused = h
        .source
        .act(Action::SaveInferenceConfig {
            mode: "hybrid".into(),
            builtin_url: url.clone(),
            builtin_model: "qwen2.5:3b".into(),
        })
        .unwrap_err();
    assert!(refused.contains("does not serve `qwen2.5:3b`"), "{refused}");
    assert!(refused.contains("qwen2.5:7b"), "{refused}");
    assert!(!h.status.lock().unwrap().local_llm_configured);

    let saved = h
        .source
        .act(Action::SaveInferenceConfig {
            mode: "hybrid".into(),
            builtin_url: url.clone(),
            builtin_model: "qwen2.5:7b".into(),
        })
        .expect("a served model saves");
    assert_eq!(saved["configured"], true);

    let discovered = h.source.query(Query::DiscoverModels).unwrap();
    let models: Vec<&str> = discovered["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m.as_str())
        .collect();
    assert_eq!(models, ["qwen2.5:0.5b", "qwen2.5:7b"], "{discovered}");

    // An endpoint nobody answers still saves: the server may just be down.
    let offline = h
        .source
        .act(Action::SaveInferenceConfig {
            mode: "local_only".into(),
            builtin_url: "http://127.0.0.1:1/v1".into(),
            builtin_model: "anything".into(),
        })
        .expect("an unreachable endpoint is not a typo");
    assert_eq!(offline["configured"], true);
}

/// The adapter supplies its executable and its models, so the settings form
/// can offer both as choices instead of free text.
#[test]
fn every_adapter_choice_carries_its_executable_and_a_model_list() {
    let h = harness();
    let choices = h.source.query(Query::ConfigChoices).unwrap();
    let adapters = choices["adapters"].as_array().unwrap();
    assert!(!adapters.is_empty());
    for adapter in adapters {
        assert!(adapter.get("executable").is_some(), "{adapter}");
        assert!(adapter["models"].is_array(), "{adapter}");
        assert!(adapter["models_source"].is_string(), "{adapter}");
    }
    let values: Vec<&str> = adapters
        .iter()
        .filter_map(|a| a["value"].as_str())
        .collect();
    assert_eq!(
        values,
        ["claude-code", "codex", "ollama", "raw-agent-loop"],
        "agents.*.adapter offers exactly what AgentAdapterKind accepts"
    );
    let runtimes: Vec<&str> = choices["runtimes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["value"].as_str())
        .collect();
    assert!(runtimes.contains(&"openai-api") && runtimes.contains(&"anthropic-api"));
    let claude = adapters
        .iter()
        .find(|a| a["value"] == "claude-code")
        .expect("claude-code is a built-in adapter");
    assert_eq!(claude["executable"], "claude");
    let models: Vec<&str> = claude["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m.as_str())
        .collect();
    assert_eq!(models, ["haiku", "opus", "sonnet"]);
    let api = choices["runtimes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["value"] == "anthropic-api")
        .expect("anthropic-api is a built-in runtime");
    assert!(api["models"].as_array().unwrap().len() >= 4);
    let raw = adapters
        .iter()
        .find(|a| a["value"] == "raw-agent-loop")
        .unwrap();
    assert_eq!(
        raw["available"], true,
        "the owned loop needs no executable on PATH"
    );
}

/// The PRD location is per repository. A project on profile defaults can set
/// it from its page: the save creates the key, the loader validates it, and
/// the document query reports the effective value beforehand.
#[test]
fn a_project_can_set_its_prd_location_without_a_global_twin() {
    let h = harness();
    let document = h.source.query(Query::ConfigDocument).unwrap();
    let defaults = &document["repository_defaults"][&h.repo];
    assert_eq!(defaults["active_dir"], "docs/prds", "{document}");
    assert_eq!(defaults["profile"], "canonical");

    std::fs::create_dir_all(std::path::Path::new(&h.repo).join("docs/prds/finished")).unwrap();
    let saved = h
        .source
        .act(Action::SaveConfig {
            edits: vec![
                ConfigEdit {
                    path: vec!["repositories".into(), h.repo.clone(), "archived_dir".into()],
                    value: "docs/prds/finished".into(),
                },
                ConfigEdit {
                    path: vec![
                        "repositories".into(),
                        h.repo.clone(),
                        "risk_vocabulary".into(),
                    ],
                    value: "persistence, routing".into(),
                },
            ],
        })
        .expect("repository-only keys are creatable");
    assert_eq!(saved["saved"], 2);
    let written: toml::Value =
        toml::from_str(&std::fs::read_to_string(&h.config).unwrap()).unwrap();
    let entry = &written["repositories"][h.repo.as_str()];
    assert_eq!(entry["archived_dir"].as_str(), Some("docs/prds/finished"));
    assert_eq!(
        entry["risk_vocabulary"].as_array().unwrap().len(),
        2,
        "{entry}"
    );
    let after = h.source.query(Query::ConfigDocument).unwrap();
    assert_eq!(
        after["repository_defaults"][&h.repo]["archived_dir"],
        "docs/prds/finished"
    );
}

/// FAM-BUG-094: a save the daemon would refuse at startup is refused here,
/// with the daemon's own message, and the file is untouched.
#[test]
fn a_save_the_daemon_would_refuse_is_refused_before_writing() {
    let h = harness();
    let before = std::fs::read_to_string(&h.config).unwrap();
    let refused = h
        .source
        .act(Action::SaveConfig {
            edits: vec![ConfigEdit {
                path: vec!["agents".into(), "reviewer".into(), "adapter".into()],
                value: "openai-api".into(),
            }],
        })
        .unwrap_err();
    assert!(refused.contains("not saved"), "{refused}");
    assert!(refused.contains("openai-api"), "{refused}");
    assert_eq!(std::fs::read_to_string(&h.config).unwrap(), before);
    assert!(
        std::fs::read_dir(h.config.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().contains("bak-window")),
        "no backup is made for a save that did not happen"
    );

    let saved = h
        .source
        .act(Action::SaveConfig {
            edits: vec![ConfigEdit {
                path: vec!["agents".into(), "reviewer".into(), "adapter".into()],
                value: "codex".into(),
            }],
        })
        .expect("a value the daemon accepts saves");
    assert_eq!(saved["saved"], 1);
}

/// Saving back to disabled has to clear the flag too, or the menu keeps
/// offering to enable a backend that no longer exists.
#[test]
fn saving_disabled_clears_the_configured_flag() {
    let h = harness();
    h.source
        .act(Action::SaveInferenceConfig {
            mode: "local_only".into(),
            builtin_url: "http://127.0.0.1:1".into(),
            builtin_model: "qwen2.5:3b".into(),
        })
        .unwrap();
    assert!(h.status.lock().unwrap().local_llm_configured);

    let result = h
        .source
        .act(Action::SaveInferenceConfig {
            mode: "disabled".into(),
            builtin_url: "http://127.0.0.1:1".into(),
            builtin_model: "qwen2.5:3b".into(),
        })
        .unwrap();

    assert_eq!(result["configured"], false);
    assert!(!h.status.lock().unwrap().local_llm_configured);
    assert!(!h.status.lock().unwrap().local_llm_enabled);
}

/// A mode that needs an endpoint must not be saved without one: the config
/// would be written, the backend would fail to load, and the operator would
/// be back to a setting that appears to do nothing.
#[test]
fn a_mode_needing_an_endpoint_is_refused_without_one() {
    let h = harness();
    for (url, model) in [("", "qwen2.5:3b"), ("http://127.0.0.1:1", "")] {
        let outcome = h.source.act(Action::SaveInferenceConfig {
            mode: "local_only".into(),
            builtin_url: url.into(),
            builtin_model: model.into(),
        });
        assert!(
            outcome.is_err(),
            "expected a refusal for ({url:?}, {model:?})"
        );
    }
    // Nothing was written on the way to refusing.
    let document = h.source.query(Query::ConfigDocument).unwrap();
    assert!(document["document"].get("inference").is_none());
}

#[test]
fn an_unknown_mode_is_refused() {
    let h = harness();
    let outcome = h.source.act(Action::SaveInferenceConfig {
        mode: "sometimes".into(),
        builtin_url: "http://127.0.0.1:1".into(),
        builtin_model: "qwen2.5:3b".into(),
    });
    assert!(outcome.is_err(), "expected a refusal, got {outcome:?}");
}

// ------------------------------------------------------------------ rounds

/// The waterfall's axis, end to end: the window asks one question and gets
/// back the sessions and every attempt inside them, scoped to this repository.
#[test]
fn the_rounds_query_returns_sessions_with_their_attempts() {
    let h = harness();
    {
        let db = h.db.lock().unwrap();
        let driver = DriverRepository::new(db.conn());
        let key = {
            use familiar_ai_core::BacklogDiscovery as _;
            familiar_ai_core::FilesystemBacklogDiscovery
                .resolve(std::path::Path::new(&h.repo))
                .unwrap()
                .key
        };
        driver.open_session("s1", &key, "{}").unwrap();
        let sequence = driver
            .record_attempt_started("s1", "PRD-1", "docs/prds/PRD-1.md", None)
            .unwrap();
        driver
            .record_attempt_finished("s1", sequence, "completed", None, None, Some(1_000))
            .unwrap();
    }

    let value = h
        .source
        .query(Query::Rounds {
            repo: h.repo.clone(),
            limit: 20,
        })
        .expect("rounds should be readable");

    assert_eq!(value["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(value["total_sessions"], 1);
    let attempts = value["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["prd_path"], "docs/prds/PRD-1.md");
    assert_eq!(attempts[0]["outcome"], "completed");
    assert_eq!(attempts[0]["session_id"], "s1");
}

/// A repository the driver has never run in must answer with an empty chart
/// rather than an error: "no rounds yet" is a state, not a failure.
#[test]
fn a_repository_with_no_sessions_returns_an_empty_chart() {
    let h = harness();
    let value = h
        .source
        .query(Query::Rounds {
            repo: h.repo.clone(),
            limit: 20,
        })
        .expect("an empty history is not an error");
    assert!(value["sessions"].as_array().unwrap().is_empty());
    assert!(value["attempts"].as_array().unwrap().is_empty());
    assert_eq!(value["total_sessions"], 0);
}
