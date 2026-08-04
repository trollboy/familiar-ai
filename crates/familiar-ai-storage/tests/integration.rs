use familiar_ai_core::models::*;
use familiar_ai_storage::*;
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

fn lifecycle_fixture(root: &std::path::Path) -> (Database, i64) {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let project = db
        .create_project(&NewProject {
            name: "lifecycle".into(),
            repo_root: root.to_str().unwrap().into(),
            ignored_paths: vec![],
            token_budget: None,
        })
        .unwrap();
    (db, project.id)
}

#[test]
fn modification_tombstones_complete_row_and_deduplicates_pending_work() {
    let tmp = tempdir().unwrap();
    let (db, pid) = lifecycle_fixture(tmp.path());
    let original = db
        .create_or_update_file_summary(&NewFileSummary {
            project_id: pid,
            path: "src/a.rs".into(),
            summary: "old".into(),
            tags: vec!["tag".into()],
            extracted_symbols: vec!["A".into()],
            last_known_mtime: Some(4),
            last_known_size: Some(8),
        })
        .unwrap();
    db.observe_change(pid, "src/a.rs", LifecycleChange::Modify, "watcher", false)
        .unwrap();
    db.observe_change(pid, "src/a.rs", LifecycleChange::Modify, "watcher", true)
        .unwrap();
    assert!(db
        .get_file_summary_by_path(pid, "src/a.rs")
        .unwrap()
        .is_none());
    assert_eq!(db.list_pending_summary_work(pid, 10).unwrap().len(), 1);
    #[allow(clippy::type_complexity)]
    let row:(i64,String,String,String,Option<String>,Option<i64>,Option<i64>,String,String,String)=db.conn().query_row("SELECT original_file_summary_id,path,summary,tags_json,extracted_symbols_json,last_known_mtime,last_known_size,last_updated_at,original_created_at,original_updated_at FROM file_summary_lifecycle_tombstones WHERE project_id=?1",[pid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))).unwrap();
    assert_eq!(row.0, original.id);
    assert_eq!(row.1, "src/a.rs");
    assert_eq!(row.2, "old");
    assert_eq!(row.3, "[\"tag\"]");
    assert_eq!(row.4.as_deref(), Some("[\"A\"]"));
    assert_eq!(row.5, Some(4));
    assert_eq!(row.6, Some(8));
    assert_eq!(row.7, original.last_updated_at.to_rfc3339());
    assert_eq!(row.8, original.created_at.to_rfc3339());
    assert_eq!(row.9, original.updated_at.to_rfc3339());
}

#[test]
fn exact_rename_is_atomic_and_never_rekeys_summary() {
    let tmp = tempdir().unwrap();
    let (db, pid) = lifecycle_fixture(tmp.path());
    db.create_or_update_file_summary(&NewFileSummary {
        project_id: pid,
        path: "old.rs".into(),
        summary: "old".into(),
        tags: vec![],
        extracted_symbols: vec![],
        last_known_mtime: None,
        last_known_size: None,
    })
    .unwrap();
    db.observe_exact_rename(pid, "old.rs", "new.rs", "watcher", false)
        .unwrap();
    assert!(db
        .get_file_summary_by_path(pid, "old.rs")
        .unwrap()
        .is_none());
    assert!(db
        .get_file_summary_by_path(pid, "new.rs")
        .unwrap()
        .is_none());
    let pending = db.list_pending_summary_work(pid, 10).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].path, "new.rs");
    let related: String = db
        .conn()
        .query_row(
            "SELECT related_path FROM file_summary_lifecycle_tombstones WHERE project_id=?1",
            [pid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(related, "new.rs");
}

#[test]
fn incomplete_scan_cannot_reconcile_absence_but_complete_empty_scan_can() {
    let tmp = tempdir().unwrap();
    let (db, pid) = lifecycle_fixture(tmp.path());
    db.create_or_update_file_summary(&NewFileSummary {
        project_id: pid,
        path: "gone.rs".into(),
        summary: "gone".into(),
        tags: vec![],
        extracted_symbols: vec![],
        last_known_mtime: None,
        last_known_size: None,
    })
    .unwrap();
    let run = db
        .start_repository_scan(pid, tmp.path().to_str().unwrap(), "prd003-v1")
        .unwrap();
    db.fail_repository_scan(run.id, "walker failed").unwrap();
    assert!(db.reconcile_repository_scan(&run).is_err());
    assert!(db
        .get_file_summary_by_path(pid, "gone.rs")
        .unwrap()
        .is_some());
    let run = db
        .start_repository_scan(pid, tmp.path().to_str().unwrap(), "prd003-v1")
        .unwrap();
    db.mark_scan_enumeration_complete(run.id).unwrap();
    db.reconcile_repository_scan(&run).unwrap();
    assert!(db
        .get_file_summary_by_path(pid, "gone.rs")
        .unwrap()
        .is_none());
    assert_eq!(
        db.latest_scan_status(pid).unwrap().unwrap().status,
        "reconciliation_complete"
    );
}

#[test]
fn lifecycle_schema_passes_sqlite_integrity_checks() {
    let tmp = tempdir().unwrap();
    let (db, _) = lifecycle_fixture(tmp.path());
    let integrity: String = db
        .conn()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    let foreign_key_errors: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(integrity, "ok");
    assert_eq!(foreign_key_errors, 0);
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
