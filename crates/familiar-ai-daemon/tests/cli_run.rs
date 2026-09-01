#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use tempfile::tempdir;

static CLI_RUN: Mutex<()> = Mutex::new(());

fn fixture_repository(root: &std::path::Path) -> std::path::PathBuf {
    let repository = root.join("repository");
    fs::create_dir_all(repository.join("docs/prds")).unwrap();
    fs::write(
        repository.join("docs/prds/PRD-001.md"),
        "# PRD-001: CLI fixture\n\n**Status:** Ready for implementation\n\n## Acceptance Criteria\n\n1. The fixture runs.\n\n## Expected Files\n\n- `src/fixture.rs`\n",
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.invalid"],
        vec!["config", "user.name", "Test"],
        vec!["add", "."],
        vec!["commit", "-qm", "fixture"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
    }
    repository
}

#[test]
fn run_feeds_fake_codex_streams_output_and_returns_its_status() {
    let _guard = CLI_RUN.lock().unwrap();
    let temp = tempdir().unwrap();
    let fake = temp.path().join("codex");
    let capture = temp.path().join("prompt.txt");
    let database = temp.path().join("familiar.db");
    let repository = fixture_repository(temp.path());
    fs::write(
        &fake,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'codex-cli 1.2.3\\n'; exit 0; fi\nprintf '%s\\n' \"$@\" > \"$FAKE_CODEX_ARGS\"\ncat > \"$FAKE_CODEX_PROMPT\"\nprintf '%s\\n' '{\"type\":\"turn.started\",\"model\":\"fake-model\"}'\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"fake stdout\"}}'\nprintf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":3,\"output_tokens\":4}}'\nprintf 'fake stderr\\n' >&2\nexit 23\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake, permissions).unwrap();
    let args = temp.path().join("args.txt");

    let output = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .current_dir(&repository)
        .args(["run", "docs/prds/PRD-001.md"])
        .env("HOME", temp.path())
        .env("XDG_RUNTIME_DIR", temp.path().join("runtime"))
        .env("PATH", format!("{}:/bin:/usr/bin", temp.path().display()))
        .env("FAKE_CODEX_ARGS", &args)
        .env("FAKE_CODEX_PROMPT", &capture)
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        )
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        )
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(23));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "fake stdout\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("pending -> in_progress actor=system:familiar-ai-run:"));
    assert!(stderr.contains("fake stderr\n"));
    assert!(stderr.contains("remains in_progress reason=implementation_failed"));
    let args = fs::read_to_string(args).unwrap();
    assert!(args.starts_with("exec\n--config\nprompt_cache_key=\"sha256:"));
    assert!(args.ends_with("\"\n--json\n-\n"));

    let prompt = fs::read_to_string(capture).unwrap();
    assert!(prompt.contains("# PRD-001: CLI fixture"));

    let db = familiar_ai_storage::Database::open(&database).unwrap();
    let rows = familiar_ai_storage::ExecutionHistoryRepository::new(db.conn())
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
    assert!(
        !temp
            .path()
            .join("runtime/familiar-ai/control-plane.claim")
            .exists(),
        "claim-holding in-process service must release ownership after legacy run"
    );

    let history = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .args(["history", "--limit", "1", "--verbose"])
        .env("HOME", temp.path())
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        )
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        )
        .output()
        .unwrap();
    assert!(history.status.success());
    let history = String::from_utf8(history.stdout).unwrap();
    assert!(history.contains("docs/prds/PRD-001.md"));
    assert!(history.contains("model: — (agent_not_reported)"));

    let usage = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .arg("usage")
        .env("HOME", temp.path())
        .env("FAMILIAR_AI_DATABASE__PATH", &database)
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        )
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        )
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
    let _guard = CLI_RUN.lock().unwrap();
    let temp = tempdir().unwrap();
    let fake = temp.path().join("codex");
    let repository = fixture_repository(temp.path());
    fs::write(
        &fake,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'codex-cli 1.2.3\\n'; exit 0; fi\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"visible early\"}}'\nsleep 2\nexit 0\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake, permissions).unwrap();

    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_familiar-ai"))
        .current_dir(&repository)
        .args(["run", "docs/prds/PRD-001.md"])
        .env("HOME", temp.path())
        .env("PATH", format!("{}:/bin:/usr/bin", temp.path().display()))
        .env(
            "FAMILIAR_AI_DATABASE__PATH",
            temp.path().join("familiar.db"),
        )
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "visible early\n");
    assert!(started.elapsed().as_millis() < 1_500);
    assert!(!child.wait().unwrap().success());
}
