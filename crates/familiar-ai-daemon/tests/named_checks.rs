use std::fs;

use familiar_ai_core::Config;
use tempfile::tempdir;

fn write_config(body: &str) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), body).unwrap();
    file
}

fn check(name: &str, command: &str) -> String {
    format!(
        "[checks.{name}]\nargv = ['{command}']\nrequired = true\ntimeout_ms = 1000\nworking_directory = '.'\n\n"
    )
}

#[test]
fn one_definition_is_referenced_by_two_repositories() {
    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    let body = format!(
        "{}[assignments]\nverification = ['fmt']\n\n[repositories.\"{}\"]\n[repositories.\"{}\".assignments]\nverification = ['fmt']\n",
        check("fmt", "/usr/bin/true"),
        first.path().display(),
        second.path().display(),
    );
    let file = write_config(&body);
    let config = Config::load(Some(file.path())).unwrap();
    assert_eq!(config.checks.len(), 1);
    for repository in [first.path(), second.path()] {
        let effective = config
            .effective_execution(&repository.canonicalize().unwrap())
            .unwrap();
        assert_eq!(effective.review.verification.len(), 1);
        assert_eq!(effective.review.verification[0].check_id, "fmt");
    }
}

#[test]
fn project_assignment_replaces_global_order_and_run_input_uses_it() {
    let repo = tempdir().unwrap();
    let body = format!(
        "{}{}[assignments]\nverification = ['fmt', 'test']\n\n[repositories.\"{}\".assignments]\nverification = ['test', 'fmt']\n",
        check("fmt", "/usr/bin/true"),
        check("test", "/usr/bin/false"),
        repo.path().display(),
    );
    let file = write_config(&body);
    let config = Config::load(Some(file.path())).unwrap();
    let effective = config
        .effective_execution(&repo.path().canonicalize().unwrap())
        .unwrap();
    assert_eq!(
        effective
            .review
            .verification
            .iter()
            .map(|value| value.check_id.as_str())
            .collect::<Vec<_>>(),
        ["test", "fmt"]
    );
}

#[test]
fn legacy_arrays_rewrite_to_named_definitions_preserving_order() {
    let repo = tempdir().unwrap();
    let body = format!(
        "[[review.verification]]\ncheck_id = 'fmt'\nargv = ['/usr/bin/true']\n\n[[review.verification]]\ncheck_id = 'test'\nargv = ['/usr/bin/false']\n\n[repositories.\"{}\"]\n[[repositories.\"{}\".review.verification]]\ncheck_id = 'test'\nargv = ['/usr/bin/false']\n",
        repo.path().display(), repo.path().display(),
    );
    let file = write_config(&body);
    let config = Config::load(Some(file.path())).unwrap();
    assert_eq!(config.assignments.verification, ["fmt", "test"]);
    assert_eq!(config.checks.len(), 2);
    assert_eq!(
        config
            .effective_execution(&repo.path().canonicalize().unwrap())
            .unwrap()
            .review
            .verification[0]
            .check_id,
        "test"
    );
}

#[test]
fn project_override_changes_fields_but_never_argv() {
    let repo = tempdir().unwrap();
    let body = format!(
        "{}[assignments]\nverification = ['fmt']\n\n[repositories.\"{}\".checks.fmt]\nrequired = false\ntimeout_ms = 42\nenvironment = {{ PATH = '/project/bin' }}\n",
        check("fmt", "/usr/bin/true"), repo.path().display(),
    );
    let file = write_config(&body);
    let config = Config::load(Some(file.path())).unwrap();
    let resolved = &config
        .effective_execution(&repo.path().canonicalize().unwrap())
        .unwrap()
        .review
        .verification[0];
    assert_eq!(resolved.argv, ["/usr/bin/true"]);
    assert!(!resolved.required);
    assert_eq!(resolved.timeout_ms, 42);
    assert_eq!(resolved.environment.get("PATH").unwrap(), "/project/bin");
}

#[test]
fn loader_rejects_unknown_empty_and_illegal_override_shapes_by_name() {
    for (body, needle) in [
        ("[assignments]\nverification=['missing']\n", "missing"),
        ("[checks.empty]\nargv=[]\n", "empty"),
        (
            "[repositories.\"/missing\".checks.unknown]\ntimeout_ms=1\n",
            "unknown",
        ),
        (
            "[checks.fmt]\nargv=['/usr/bin/true']\n[repositories.\"/missing\".checks.fmt]\nargv=['/usr/bin/false']\n",
            "argv",
        ),
    ] {
        let file = write_config(body);
        let error = Config::load(Some(file.path())).unwrap_err().to_string();
        assert!(error.contains(needle), "{error}");
    }
}

#[test]
fn split_defaults_and_contract_publish_the_named_check_shape() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let checks = fs::read_to_string(root.join("config/default.d/checks.toml")).unwrap();
    let assignments = fs::read_to_string(root.join("config/default.d/assignments.toml")).unwrap();
    let review = fs::read_to_string(root.join("config/default.d/review.toml")).unwrap();
    let contract = fs::read_to_string(root.join("docs/contracts/verification-checks.md")).unwrap();

    assert!(checks.contains("[checks.fmt]"));
    assert!(assignments.contains("[assignments]"));
    assert!(!review.contains("# [[review.verification]]"));
    assert!(contract.contains("repositories.\"/work/project\".checks.fmt"));
    assert!(contract.contains("assignments.verification"));

    Config::load_defaults_dir(&root.join("config/default.d")).unwrap();
}
