use std::fs;
use std::process::Command;

use familiar_ai_daemon::config_cli::{
    effective_config_for_repository, execute_with_context, ConfigAction, ConfigContext,
};

#[test]
fn exact_snapshot_is_the_only_project_configuration_with_authority() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    fs::create_dir(&repository).unwrap();
    let context = ConfigContext {
        config_path: directory.path().join("config.toml"),
        data_dir: directory.path().join("data"),
    };
    fs::write(
        repository.join("familiar.toml"),
        "profile='numbered-slug'\n",
    )
    .unwrap();

    let before = effective_config_for_repository(&context, &repository).unwrap();
    assert_eq!(before.repository(&repository).unwrap().profile, "canonical");

    execute_with_context(
        ConfigAction::ProjectApprove {
            repository: repository.clone(),
            actor: "human:operator".into(),
        },
        &context,
    )
    .unwrap();
    let canonical = repository.canonicalize().unwrap();
    let approved = effective_config_for_repository(&context, &repository).unwrap();
    assert_eq!(
        approved.repository(&canonical).unwrap().profile,
        "numbered-slug"
    );

    fs::write(repository.join("familiar.toml"), "profile='canonical'\n").unwrap();
    let drifted = effective_config_for_repository(&context, &repository).unwrap();
    assert_eq!(drifted.repository(&canonical).unwrap().profile, "canonical");
}

#[test]
fn repository_without_project_file_retains_legacy_resolution() {
    let directory = tempfile::tempdir().unwrap();
    let context = ConfigContext {
        config_path: directory.path().join("config.toml"),
        data_dir: directory.path().join("data"),
    };
    let legacy = familiar_ai_core::Config::load(Some(&context.config_path)).unwrap();
    let effective = effective_config_for_repository(&context, directory.path()).unwrap();
    assert_eq!(
        legacy.repository(directory.path()).unwrap(),
        effective.repository(directory.path()).unwrap()
    );
}

#[test]
fn nested_directory_uses_root_snapshot_and_approval_identity() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    let nested = repository.join("src/nested");
    fs::create_dir_all(&nested).unwrap();
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .arg(&repository)
        .status()
        .unwrap()
        .success());
    fs::write(
        repository.join("familiar.toml"),
        "profile='numbered-slug'\n",
    )
    .unwrap();
    let context = ConfigContext {
        config_path: directory.path().join("config.toml"),
        data_dir: directory.path().join("data"),
    };

    execute_with_context(
        ConfigAction::ProjectApprove {
            repository: nested.clone(),
            actor: "human:operator".into(),
        },
        &context,
    )
    .unwrap();

    let effective = effective_config_for_repository(&context, &nested).unwrap();
    assert_eq!(
        effective.repository(&repository).unwrap().profile,
        "numbered-slug"
    );
    execute_with_context(ConfigAction::Status { repository: nested }, &context).unwrap();
}
