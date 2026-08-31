use std::fs;

use familiar_ai_core::Config;
use familiar_ai_daemon::config_cli::{execute_with_context, ConfigAction, ConfigContext};
use familiar_ai_daemon::run::{
    resolved_agent_entries, resolved_remediation_entry, resolved_worker_plan, RouteContext,
};
use familiar_ai_storage::{ConfigDecisionRepository, Database};

fn fixture(body: &str) -> (tempfile::TempDir, ConfigContext) {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    let database = temp.path().join("state.db");
    fs::write(
        &config_path,
        format!(
            "[database]\npath = {:?}\n\n{body}",
            database.to_str().unwrap()
        ),
    )
    .unwrap();
    let context = ConfigContext {
        config_path,
        data_dir: temp.path().to_path_buf(),
    };
    (temp, context)
}

#[test]
fn migration_preserves_resolved_roles_comments_backup_and_audit() {
    let (_temp, context) = fixture(
        "# preserved outside agents\n[logging]\nlevel = \"info\"\n\n[agents.implementation]\nadapter = \"claude-code\"\nexecutable = \"claude-impl\"\nmodel = \"sonnet\"\npermission_mode = \"acceptEdits\"\n\n[agents.reviewer]\nadapter = \"codex\"\nexecutable = \"codex-review\"\nmodel = \"gpt-review\"\n",
    );
    let before_bytes = fs::read_to_string(&context.config_path).unwrap();
    let before = Config::load(Some(&context.config_path)).unwrap();
    let (implementation, reviewer) = resolved_agent_entries(&before).unwrap();
    let remediation = resolved_remediation_entry(&before).unwrap();

    execute_with_context(
        ConfigAction::MigrateAgents {
            actor: Some("human:migration-test".into()),
        },
        &context,
    )
    .unwrap();

    let after_bytes = fs::read_to_string(&context.config_path).unwrap();
    assert!(after_bytes.contains("# preserved outside agents"));
    assert!(!after_bytes.contains("[agents"));
    assert_eq!(
        fs::read_to_string(context.config_path.with_extension("toml.bak")).unwrap(),
        before_bytes
    );
    let mut after = Config::load(Some(&context.config_path)).unwrap();
    // Force the review stage into the equivalence oracle; review policy is
    // otherwise intentionally disabled in this minimal migration fixture.
    after.review.enabled = true;
    let plan = resolved_worker_plan(&after, &RouteContext::default()).unwrap();
    assert_eq!(plan.0, implementation);
    assert_eq!(plan.1, reviewer);
    assert_eq!(resolved_remediation_entry(&after).unwrap(), remediation);
    let db = Database::open(after.database.path.as_ref().unwrap()).unwrap();
    db.run_migrations().unwrap();
    let rows = ConfigDecisionRepository::new(&db).list(10).unwrap();
    assert_eq!(rows[0].actor, "human:migration-test");
    assert_eq!(rows[0].command, "familiar-ai config migrate agents");

    let migrated_bytes = fs::read_to_string(&context.config_path).unwrap();
    execute_with_context(
        ConfigAction::MigrateAgents {
            actor: Some("human:migration-test".into()),
        },
        &context,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&context.config_path).unwrap(),
        migrated_bytes
    );
}

#[test]
fn first_registry_enable_against_legacy_agents_leaves_original_untouched() {
    let (_temp, context) = fixture(
        "[agents]\n\n[providers.local]\nkind = \"inference\"\nhost = \"localhost:1\"\nauth = \"none\"\nmodels = [\"m\"]\n",
    );
    let before = fs::read_to_string(&context.config_path).unwrap();
    let result = execute_with_context(
        ConfigAction::ModelEnable {
            model: "local/m".into(),
            capabilities: vec!["implementation".into()],
            actor: Some("human:test".into()),
        },
        &context,
    );
    assert!(result.unwrap_err().contains("config migrate agents"));
    assert_eq!(fs::read_to_string(&context.config_path).unwrap(), before);
    execute_with_context(
        ConfigAction::MigrateAgents {
            actor: Some("human:test".into()),
        },
        &context,
    )
    .unwrap();
    execute_with_context(
        ConfigAction::ModelEnable {
            model: "local/m".into(),
            capabilities: vec!["implementation".into()],
            actor: Some("human:test".into()),
        },
        &context,
    )
    .unwrap();
    assert!(Config::load(Some(&context.config_path))
        .unwrap()
        .worker_registry
        .unwrap()
        .workers
        .contains_key("local/m"));
}

#[test]
fn migration_no_ops_change_no_bytes_or_backup() {
    let (_temp, context) = fixture("# no agents\n");
    let before = fs::read_to_string(&context.config_path).unwrap();
    execute_with_context(
        ConfigAction::MigrateAgents {
            actor: Some("human:test".into()),
        },
        &context,
    )
    .unwrap();
    assert_eq!(fs::read_to_string(&context.config_path).unwrap(), before);
    assert!(!context.config_path.with_extension("toml.bak").exists());
}

#[test]
fn interrupted_migration_leaves_original_configuration_untouched() {
    let (temp, context) = fixture("[agents]\n");
    let database_directory = temp.path().join("database-is-a-directory");
    fs::create_dir(&database_directory).unwrap();
    let before = fs::read_to_string(&context.config_path).unwrap();
    let original_database = temp.path().join("state.db").to_string_lossy().into_owned();
    let invalid_database = database_directory.to_string_lossy().into_owned();
    fs::write(
        &context.config_path,
        before.replace(&original_database, &invalid_database),
    )
    .unwrap();
    let before = fs::read_to_string(&context.config_path).unwrap();
    let result = execute_with_context(
        ConfigAction::MigrateAgents {
            actor: Some("human:test".into()),
        },
        &context,
    );
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&context.config_path).unwrap(), before);
}
