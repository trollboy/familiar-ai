#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::Instant;

use tempfile::tempdir;

#[test]
fn run_feeds_fake_codex_streams_output_and_returns_its_status() {
    let temp = tempdir().unwrap();
    let fake = temp.path().join("codex");
    let capture = temp.path().join("prompt.txt");
    let database = temp.path().join("familiar.db");
    fs::write(
        &fake,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'codex-cli 1.2.3\\n'; exit 0; fi\nprintf '%s\\n' \"$@\" > \"$FAKE_CODEX_ARGS\"\ncat > \"$FAKE_CODEX_PROMPT\"\nprintf '%s\\n' '{\"type\":\"turn.started\",\"model\":\"fake-model\"}'\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"fake stdout\"}}'\nprintf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":3,\"output_tokens\":4}}'\nprintf 'fake stderr\\n' >&2\nexit 23\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake, permissions).unwrap();
    let args = temp.path().join("args.txt");

    let output = Command::new(env!("CARGO_BIN_EXE_familiar"))
        .current_dir(env!("CARGO_MANIFEST_DIR").to_owned() + "/../..")
        .args(["run", "docs/prds/PRD-003.md"])
        .env("PATH", format!("{}:/bin:/usr/bin", temp.path().display()))
        .env("FAKE_CODEX_ARGS", &args)
        .env("FAKE_CODEX_PROMPT", &capture)
        .env("FAMILIAR_DATABASE__PATH", &database)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(23));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "fake stdout\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "fake stderr\n");
    assert_eq!(fs::read_to_string(args).unwrap(), "exec\n--json\n-\n");

    let prompt = fs::read_to_string(capture).unwrap();
    assert!(prompt.contains("# PRD-003: Repository Lifecycle and Scan Reconciliation"));
    assert!(prompt.contains("## Directly referenced document: docs/contracts/command-model.md"));
    assert!(prompt.contains("## Directly referenced document: docs/contracts/query-model.md"));
    assert!(prompt.contains("## Directly referenced document: docs/contracts/event-model.md"));

    let db = familiar_storage::Database::open(&database).unwrap();
    let rows = familiar_storage::ExecutionHistoryRepository::new(db.conn())
        .recent(20)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, "failed");
    assert_eq!(rows[0].exit_code, Some(23));
    assert_eq!(rows[0].model, None);
    assert_eq!(
        rows[0].unavailable_fields.get("model").map(String::as_str),
        Some("agent_not_reported")
    );
    assert_eq!(rows[0].input_tokens, Some(10));
    assert_eq!(rows[0].cached_tokens, Some(3));
    assert_eq!(rows[0].output_tokens, Some(4));
    assert_eq!(rows[0].total_tokens, Some(14));

    let history = Command::new(env!("CARGO_BIN_EXE_familiar"))
        .args(["history", "--limit", "1", "--verbose"])
        .env("FAMILIAR_DATABASE__PATH", &database)
        .output()
        .unwrap();
    assert!(history.status.success());
    let history = String::from_utf8(history.stdout).unwrap();
    assert!(history.contains("docs/prds/PRD-003.md"));
    assert!(history.contains("model: — (agent_not_reported)"));

    let usage = Command::new(env!("CARGO_BIN_EXE_familiar"))
        .arg("usage")
        .env("FAMILIAR_DATABASE__PATH", &database)
        .output()
        .unwrap();
    assert!(usage.status.success());
    let usage = String::from_utf8(usage.stdout).unwrap();
    assert!(usage.contains("Executions: 1"));
    assert!(usage.contains("Known input tokens: 10"));
    assert!(usage.contains("Executions with unknown cost: 1"));
}

#[test]
fn structured_output_is_forwarded_before_fake_codex_exits() {
    let temp = tempdir().unwrap();
    let fake = temp.path().join("codex");
    fs::write(
        &fake,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'codex-cli 1.2.3\\n'; exit 0; fi\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"visible early\"}}'\nsleep 2\nexit 0\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake, permissions).unwrap();

    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_familiar"))
        .current_dir(env!("CARGO_MANIFEST_DIR").to_owned() + "/../..")
        .args(["run", "docs/prds/PRD-004.md"])
        .env("PATH", format!("{}:/bin:/usr/bin", temp.path().display()))
        .env("FAMILIAR_DATABASE__PATH", temp.path().join("familiar.db"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "visible early\n");
    assert!(started.elapsed().as_millis() < 1_500);
    assert!(child.wait().unwrap().success());
}
