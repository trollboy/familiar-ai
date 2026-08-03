#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use tempfile::tempdir;

#[test]
fn run_feeds_fake_codex_streams_output_and_returns_its_status() {
    let temp = tempdir().unwrap();
    let fake = temp.path().join("codex");
    let capture = temp.path().join("prompt.txt");
    fs::write(
        &fake,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$FAKE_CODEX_ARGS\"\ncat > \"$FAKE_CODEX_PROMPT\"\nprintf 'fake stdout\\n'\nprintf 'fake stderr\\n' >&2\nexit 23\n",
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
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(23));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "fake stdout\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "fake stderr\n");
    assert_eq!(fs::read_to_string(args).unwrap(), "exec\n-\n");

    let prompt = fs::read_to_string(capture).unwrap();
    assert!(prompt.contains("# PRD-003: Repository Lifecycle and Scan Reconciliation"));
    assert!(prompt.contains("## Directly referenced document: docs/contracts/command-model.md"));
    assert!(prompt.contains("## Directly referenced document: docs/contracts/query-model.md"));
    assert!(prompt.contains("## Directly referenced document: docs/contracts/event-model.md"));
}
