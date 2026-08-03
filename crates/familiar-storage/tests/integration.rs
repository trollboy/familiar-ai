use familiar_core::models::*;
use familiar_storage::*;
use tempfile::tempdir;

#[test]
fn file_based_db_lifecycle() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("test.db");

    // Open, migrate, create data
    {
        let db = Database::open(&db_path).unwrap();
        db.run_migrations().unwrap();

        let project = db
            .create_project(&NewProject {
                name: "test".into(),
                repo_root: "/test".into(),
                ignored_paths: vec!["target/".into()],
                token_budget: Some(1000),
            })
            .unwrap();

        db.create_or_update_file_summary(&NewFileSummary {
            project_id: project.id,
            path: "src/main.rs".into(),
            summary: "Entry point".into(),
            tags: vec!["entry".into()],
            extracted_symbols: vec![],
            last_known_mtime: None,
            last_known_size: None,
        })
        .unwrap();

        db.create_decision(&NewDecision {
            project_id: project.id,
            title: "Use SQLite".into(),
            summary: "Simple and reliable".into(),
            related_files: vec!["Cargo.toml".into()],
            source_session: None,
            confidence: None,
        })
        .unwrap();

        db.create_session_rollup(&NewSessionRollup {
            project_id: project.id,
            summary: "Set up storage layer".into(),
            related_files: vec![],
            next_steps: vec!["Add tests".into()],
        })
        .unwrap();
    }

    // Reopen and verify data persisted
    {
        let db = Database::open(&db_path).unwrap();
        let applied = db.run_migrations().unwrap();
        assert_eq!(applied, 0, "migrations should already be applied");

        let project = db.get_project_by_repo_root("/test").unwrap().unwrap();
        assert_eq!(project.name, "test");
        assert_eq!(project.ignored_paths, vec!["target/"]);

        let summaries = db.list_file_summaries_by_project(project.id).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].summary, "Entry point");

        let decisions = db.list_decisions_by_project(project.id, 100).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].title, "Use SQLite");

        let rollups = db.list_session_rollups_by_project(project.id, 100).unwrap();
        assert_eq!(rollups.len(), 1);
        assert_eq!(rollups[0].next_steps, vec!["Add tests"]);
    }
}

#[test]
fn foreign_key_cascade_deletes_children() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();

    let project = db
        .create_project(&NewProject {
            name: "cascade-test".into(),
            repo_root: "/cascade".into(),
            ignored_paths: vec![],
            token_budget: None,
        })
        .unwrap();

    db.create_or_update_file_summary(&NewFileSummary {
        project_id: project.id,
        path: "src/lib.rs".into(),
        summary: "Library".into(),
        tags: vec![],
        extracted_symbols: vec![],
        last_known_mtime: None,
        last_known_size: None,
    })
    .unwrap();

    db.create_decision(&NewDecision {
        project_id: project.id,
        title: "Test decision".into(),
        summary: "For cascade test".into(),
        related_files: vec![],
        source_session: None,
        confidence: None,
    })
    .unwrap();

    db.create_session_rollup(&NewSessionRollup {
        project_id: project.id,
        summary: "Test rollup".into(),
        related_files: vec![],
        next_steps: vec![],
    })
    .unwrap();

    // Delete the project
    db.delete_project(project.id).unwrap();

    // All children should be gone
    assert!(db
        .list_file_summaries_by_project(project.id)
        .unwrap()
        .is_empty());
    assert!(db
        .list_decisions_by_project(project.id, 100)
        .unwrap()
        .is_empty());
    assert!(db
        .list_session_rollups_by_project(project.id, 100)
        .unwrap()
        .is_empty());
}

#[test]
fn json_fields_round_trip() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();

    let paths = vec![
        "target/".to_string(),
        ".git/".to_string(),
        "node_modules/".to_string(),
    ];

    let project = db
        .create_project(&NewProject {
            name: "json-test".into(),
            repo_root: "/json-test".into(),
            ignored_paths: paths.clone(),
            token_budget: None,
        })
        .unwrap();

    assert_eq!(project.ignored_paths, paths);

    let fetched = db.get_project_by_id(project.id).unwrap().unwrap();
    assert_eq!(fetched.ignored_paths, paths);

    // Test tags
    let tags = vec![
        "auth".to_string(),
        "middleware".to_string(),
        "security".to_string(),
    ];
    let summary = db
        .create_or_update_file_summary(&NewFileSummary {
            project_id: project.id,
            path: "src/auth.rs".into(),
            summary: "Auth module".into(),
            tags: tags.clone(),
            extracted_symbols: vec![],
            last_known_mtime: None,
            last_known_size: None,
        })
        .unwrap();
    assert_eq!(summary.tags, tags);

    // Test related_files and next_steps
    let related = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
    let next = vec!["Write tests".to_string(), "Update docs".to_string()];
    let rollup = db
        .create_session_rollup(&NewSessionRollup {
            project_id: project.id,
            summary: "Test".into(),
            related_files: related.clone(),
            next_steps: next.clone(),
        })
        .unwrap();
    assert_eq!(rollup.related_files, related);
    assert_eq!(rollup.next_steps, next);
}
