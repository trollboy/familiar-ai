use rusqlite::Connection;

use familiar_ai_core::FamiliarError;

struct Migration {
    version: i64,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/001_init.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/002_summaries_decisions.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("../migrations/003_file_summary_reconciliation.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("../migrations/004_repository_lifecycle.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("../migrations/005_execution_history.sql"),
    },
    Migration {
        version: 6,
        sql: include_str!("../migrations/006_review_cycles.sql"),
    },
    Migration {
        version: 7,
        sql: include_str!("../migrations/007_backlog.sql"),
    },
    Migration {
        version: 8,
        sql: include_str!("../migrations/008_backlog_bootstrap.sql"),
    },
    Migration {
        version: 9,
        sql: include_str!("../migrations/009_backlog_recovery.sql"),
    },
    Migration {
        version: 10,
        sql: include_str!("../migrations/010_driver_sessions.sql"),
    },
    Migration {
        version: 11,
        sql: include_str!("../migrations/011_backlog_recovery_recorded_complete.sql"),
    },
    Migration {
        version: 12,
        sql: include_str!("../migrations/012_backlog_profiles.sql"),
    },
    Migration {
        version: 13,
        sql: include_str!("../migrations/013_execution_terminal_outcomes.sql"),
    },
    Migration {
        version: 14,
        sql: include_str!("../migrations/014_driver_diagnostics.sql"),
    },
    Migration {
        version: 15,
        sql: include_str!("../migrations/015_driver_session_detail.sql"),
    },
    Migration {
        version: 16,
        sql: include_str!("../migrations/016_attempt_configuration_sources.sql"),
    },
    Migration {
        version: 17,
        sql: include_str!("../migrations/017_execution_checkpoints.sql"),
    },
    Migration {
        version: 18,
        sql: include_str!("../migrations/018_driver_components.sql"),
    },
    Migration {
        version: 19,
        sql: include_str!("../migrations/019_review_tiers.sql"),
    },
    Migration {
        version: 20,
        sql: include_str!("../migrations/020_worker_selections.sql"),
    },
    Migration {
        version: 21,
        sql: include_str!("../migrations/021_delivery_policy.sql"),
    },
    Migration {
        version: 22,
        sql: include_str!("../migrations/022_planner_batches.sql"),
    },
    Migration {
        version: 23,
        sql: include_str!("../migrations/023_worker_selection_routing_inputs.sql"),
    },
    Migration {
        version: 24,
        sql: include_str!("../migrations/024_config_decisions.sql"),
    },
    Migration {
        version: 25,
        sql: include_str!("../migrations/025_selection_decisions.sql"),
    },
    Migration {
        version: 26,
        sql: include_str!("../migrations/026_resource_decisions_and_typed_approval.sql"),
    },
    Migration {
        version: 27,
        sql: include_str!("../migrations/027_verification_escalations.sql"),
    },
    Migration {
        version: 28,
        sql: include_str!("../migrations/028_internal_delivery_targets.sql"),
    },
    Migration {
        version: 29,
        sql: include_str!("../migrations/029_project_config_approvals.sql"),
    },
    Migration {
        version: 30,
        sql: include_str!("../migrations/030_usage_observation_ledger.sql"),
    },
    Migration {
        version: 31,
        sql: include_str!("../migrations/031_integration_orchestration.sql"),
    },
    Migration {
        version: 32,
        sql: include_str!("../migrations/032_verification_truth.sql"),
    },
    Migration {
        version: 39,
        sql: include_str!("../migrations/039_anthropic_billing.sql"),
    },
    Migration {
        version: 40,
        sql: include_str!("../migrations/040_openai_accounting.sql"),
    },
    Migration {
        version: 41,
        sql: include_str!("../migrations/041_worker_spec_identity.sql"),
    },
    Migration {
        version: 42,
        sql: include_str!("../migrations/042_typed_resource_reservations.sql"),
    },
    Migration {
        version: 43,
        sql: include_str!("../migrations/043_token_compression.sql"),
    },
    Migration {
        version: 44,
        sql: include_str!("../migrations/044_context_service.sql"),
    },
    Migration {
        version: 45,
        sql: include_str!("../migrations/045_model_probation.sql"),
    },
    Migration {
        version: 47,
        sql: include_str!("../migrations/047_project_usage_series.sql"),
    },
    Migration {
        version: 49,
        sql: include_str!("../migrations/049_control_plane.sql"),
    },
    Migration {
        version: 51,
        sql: include_str!("../migrations/051_model_artifact_registry.sql"),
    },
    Migration {
        version: 52,
        sql: include_str!("../migrations/052_autonomous_delivery_decisions.sql"),
    },
    Migration {
        version: 53,
        sql: include_str!("../migrations/053_review_capability_probes.sql"),
    },
];

pub fn run_migrations(conn: &Connection) -> familiar_ai_core::Result<usize> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );",
    )
    .map_err(|e| FamiliarError::Database(format!("failed to create schema_migrations: {e}")))?;

    let applied: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        let result = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| FamiliarError::Database(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        result
    };

    let mut count = 0;
    for migration in MIGRATIONS {
        if !applied.contains(&migration.version) {
            tracing::info!(version = migration.version, "applying migration");
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| FamiliarError::Database(e.to_string()))?;
            tx.execute_batch(migration.sql).map_err(|e| {
                FamiliarError::Database(format!("migration {} failed: {e}", migration.version))
            })?;
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, datetime('now'))",
                [migration.version],
            )
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
            tx.commit()
                .map_err(|e| FamiliarError::Database(e.to_string()))?;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    fn test_db() -> crate::Database {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    #[test]
    fn migration_runs_cleanly() {
        let db = test_db();
        let tables: Vec<String> = {
            let mut stmt = db
                .conn()
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert!(tables.contains(&"projects".to_string()));
        assert!(tables.contains(&"file_summaries".to_string()));
        assert!(tables.contains(&"decisions".to_string()));
        assert!(tables.contains(&"session_rollups".to_string()));
        assert!(tables.contains(&"schema_migrations".to_string()));
        assert!(tables.contains(&"file_summary_reconciliation_runs".to_string()));
        assert!(tables.contains(&"file_summary_reconciliation_records".to_string()));
        assert!(tables.contains(&"file_summary_reconciliation_rollbacks".to_string()));
        assert!(tables.contains(&"file_summary_reconciliation_run_reasons".to_string()));
        assert!(tables.contains(&"backlog_prds".to_string()));
        assert!(tables.contains(&"backlog_status_events".to_string()));
        assert!(tables.contains(&"backlog_bootstrap_runs".to_string()));
        assert!(tables.contains(&"backlog_bootstrap_items".to_string()));
        assert!(tables.contains(&"backlog_bootstrap_rollbacks".to_string()));
        assert!(tables.contains(&"backlog_bootstrap_rollback_items".to_string()));
        assert!(tables.contains(&"backlog_recovery_events".to_string()));
        assert!(tables.contains(&"review_capability_probes".to_string()));
        let backlog_rows: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM backlog_prds", [], |row| row.get(0))
            .unwrap();
        assert_eq!(backlog_rows, 0);
        for table in [
            "review_tasks",
            "review_artifacts",
            "review_cycles",
            "review_stage_executions",
            "review_findings",
            "review_finding_events",
            "review_verification_evidence",
            "review_finding_waivers",
            "review_tier_selections",
            "worker_selections",
            "planner_batches",
            "lesson_proposals",
            "lesson_proposal_events",
        ] {
            assert!(tables.contains(&table.to_string()), "missing {table}");
        }
    }

    #[test]
    fn migration_is_idempotent() {
        let db = crate::Database::open_in_memory().unwrap();
        let first = db.run_migrations().unwrap();
        let second = db.run_migrations().unwrap();
        assert_eq!(first, 44);
        assert_eq!(second, 0);
    }

    #[test]
    fn schema_migrations_records_version() {
        let db = test_db();
        let versions: Vec<i64> = {
            let mut stmt = db
                .conn()
                .prepare("SELECT version FROM schema_migrations ORDER BY version")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            versions,
            vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25, 26, 27, 28, 29, 30, 31, 32, 39, 40, 41, 42, 43, 44, 45, 47, 49, 51, 52, 53
            ]
        );
    }

    #[test]
    fn review_capability_probe_migrates_populated_worker_specs() {
        let db = crate::Database::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
        let before_review_probes = super::MIGRATIONS.len() - 1;
        for migration in &super::MIGRATIONS[..before_review_probes] {
            db.conn().execute_batch(migration.sql).unwrap();
            db.conn()
                .execute(
                    "INSERT INTO schema_migrations(version,applied_at) VALUES(?1,'before')",
                    [migration.version],
                )
                .unwrap();
        }
        let spec = format!("wspec-sha256:{}", "b".repeat(64));
        db.conn().execute(
            "INSERT INTO worker_specs(
                spec_identity,worker_alias,provider_id,runtime_id,model_state,
                model_id,model_artifact_id,auth_profile_id,capability_profile_id,created_at
             ) VALUES(?1,'ollama-review','local','ollama','known','llama3:latest',NULL,NULL,'review','before')",
            [&spec],
        ).unwrap();

        assert_eq!(db.run_migrations().unwrap(), 1);
        db.conn()
            .execute(
                "INSERT INTO review_capability_probes(
                spec_identity,structured_output,native_tool_calling,protocol,
                runtime_version,provenance,probed_at
             ) VALUES(?1,1,1,'familiar-ai-review-v1','0.13.0','probed','after')",
                [&spec],
            )
            .unwrap();
        let retained: String = db
            .conn()
            .query_row(
                "SELECT worker_alias FROM worker_specs WHERE spec_identity=?1",
                [&spec],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained, "ollama-review");
    }

    #[test]
    fn migration_53_repairs_the_transient_version_52_collision() {
        let db = crate::Database::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
        let version_52 = super::MIGRATIONS
            .iter()
            .position(|migration| migration.version == 52)
            .unwrap();
        for migration in &super::MIGRATIONS[..version_52] {
            db.conn().execute_batch(migration.sql).unwrap();
            db.conn()
                .execute(
                    "INSERT INTO schema_migrations(version,applied_at) VALUES(?1,'before')",
                    [migration.version],
                )
                .unwrap();
        }
        db.conn()
            .execute_batch(
                "CREATE TABLE review_capability_probes (
                    spec_identity TEXT PRIMARY KEY REFERENCES worker_specs(spec_identity),
                    structured_output INTEGER NOT NULL CHECK(structured_output IN (0,1)),
                    native_tool_calling INTEGER NOT NULL CHECK(native_tool_calling IN (0,1)),
                    protocol TEXT NOT NULL,
                    runtime_version TEXT NOT NULL,
                    provenance TEXT NOT NULL CHECK(provenance IN ('probed','observed')),
                    probed_at TEXT NOT NULL
                );
                INSERT INTO schema_migrations(version,applied_at) VALUES(52,'collision');",
            )
            .unwrap();

        assert_eq!(db.run_migrations().unwrap(), 1);
        let selection_schema: String = db
            .conn()
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='driver_selection_decisions'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(selection_schema.contains("dependency_not_integrated"));
        assert!(db
            .conn()
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='review_capability_probes'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .is_ok());
    }

    #[test]
    fn artifact_registry_migrates_existing_ollama_spec_without_rewriting_it() {
        let db = crate::Database::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
        let before_artifact_registry = super::MIGRATIONS
            .iter()
            .position(|migration| migration.version == 51)
            .unwrap();
        for migration in &super::MIGRATIONS[..before_artifact_registry] {
            db.conn().execute_batch(migration.sql).unwrap();
            db.conn()
                .execute(
                    "INSERT INTO schema_migrations(version,applied_at) VALUES(?1,'before')",
                    [migration.version],
                )
                .unwrap();
        }
        let spec = format!("wspec-sha256:{}", "a".repeat(64));
        db.conn().execute(
            "INSERT INTO worker_specs(
                spec_identity,worker_alias,provider_id,runtime_id,model_state,
                model_id,model_artifact_id,auth_profile_id,capability_profile_id,created_at
             ) VALUES(?1,'ollama-review','local','ollama','known','llama3:latest',NULL,NULL,'review','before')",
            [&spec],
        ).unwrap();

        assert_eq!(db.run_migrations().unwrap(), 3);
        let artifact_id = format!("sha256:{}", "a".repeat(64));
        let migrated: (String, String) = db
            .conn()
            .query_row(
                "SELECT a.model_artifact_id,m.verification_state
             FROM model_artifact_aliases a
             JOIN model_artifacts m USING(model_artifact_id)
             WHERE a.alias='ollama-review'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(migrated, (artifact_id, "degraded-unverified-alias".into()));
        let historical_artifact: Option<String> = db
            .conn()
            .query_row(
                "SELECT model_artifact_id FROM worker_specs WHERE spec_identity=?1",
                [&spec],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(historical_artifact, None);
        assert!(db.conn().execute(
            "UPDATE worker_specs SET model_artifact_id='sha256:forbidden' WHERE spec_identity=?1",
            [&spec],
        ).is_err());
    }

    #[test]
    fn version_two_database_upgrades_additively_without_rewriting_summaries() {
        let db = crate::Database::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
        for migration in &super::MIGRATIONS[..2] {
            db.conn().execute_batch(migration.sql).unwrap();
            db.conn()
                .execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 'before')",
                    params![migration.version],
                )
                .unwrap();
        }
        db.conn()
            .execute(
                "INSERT INTO projects \
                 (id, name, repo_root, active, last_used_at, ignored_paths_json, created_at, updated_at) \
                 VALUES (1, 'legacy', '/legacy', 1, 'before', '[]', 'before', 'before')",
                [],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO file_summaries \
                 (id, project_id, path, summary, tags_json, extracted_symbols_json, \
                  last_updated_at, created_at, updated_at) \
                 VALUES (7, 1, '/legacy/src/main.rs', 'legacy payload', '[]', '[]', \
                         'before', 'before', 'before')",
                [],
            )
            .unwrap();

        assert_eq!(db.run_migrations().unwrap(), 42);
        let unchanged: (i64, String, String) = db
            .conn()
            .query_row(
                "SELECT id, path, summary FROM file_summaries WHERE id = 7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            unchanged,
            (7, "/legacy/src/main.rs".into(), "legacy payload".into())
        );
        let records: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM file_summary_reconciliation_records",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            records, 0,
            "schema upgrade must not synthesize legacy history"
        );
    }

    #[test]
    fn exact_pre_backlog_database_upgrades_without_fabricating_backlog_rows() {
        let db = crate::Database::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
        for migration in &super::MIGRATIONS[..6] {
            db.conn().execute_batch(migration.sql).unwrap();
            db.conn()
                .execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 'before')",
                    params![migration.version],
                )
                .unwrap();
        }
        db.conn()
            .execute(
                "INSERT INTO projects
                 (id, name, repo_root, active, last_used_at, ignored_paths_json, created_at, updated_at)
                 VALUES (42, 'preserved', '/preserved', 1, 'before', '[]', 'before', 'before')",
                [],
            )
            .unwrap();

        assert_eq!(db.run_migrations().unwrap(), 38);
        let project: (String, String) = db
            .conn()
            .query_row(
                "SELECT name, repo_root FROM projects WHERE id = 42",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(project, ("preserved".into(), "/preserved".into()));
        let backlog_rows: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM backlog_prds", [], |row| row.get(0))
            .unwrap();
        assert_eq!(backlog_rows, 0);
    }

    #[test]
    fn exact_post_backlog_database_upgrades_without_bootstrap_evidence() {
        let db = crate::Database::open_in_memory().unwrap();
        db.conn().execute_batch("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);").unwrap();
        for migration in &super::MIGRATIONS[..7] {
            db.conn().execute_batch(migration.sql).unwrap();
            db.conn()
                .execute(
                    "INSERT INTO schema_migrations(version,applied_at) VALUES(?1,'before')",
                    params![migration.version],
                )
                .unwrap();
        }
        db.conn().execute("INSERT INTO backlog_prds(repository_key,prd_path,prd_number,content_hash,status,discovered_at,last_seen_at,created_at,updated_at) VALUES('repo','docs/prds/PRD-009.md',9,'hash','pending','before','before','before','before')",[]).unwrap();
        assert_eq!(db.run_migrations().unwrap(), 37);
        let preserved: String = db
            .conn()
            .query_row("SELECT status FROM backlog_prds", [], |r| r.get(0))
            .unwrap();
        let runs: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM backlog_bootstrap_runs", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!((preserved, runs), ("pending".into(), 0));
    }

    #[test]
    fn migration_widens_recovery_action_set_and_preserves_prior_rows_and_foreign_keys() {
        let db = crate::Database::open_in_memory().unwrap();
        db.conn().execute_batch("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);").unwrap();
        for migration in &super::MIGRATIONS[..10] {
            db.conn().execute_batch(migration.sql).unwrap();
            db.conn()
                .execute(
                    "INSERT INTO schema_migrations(version,applied_at) VALUES(?1,'before')",
                    params![migration.version],
                )
                .unwrap();
        }
        db.conn().execute("INSERT INTO backlog_prds(repository_key,prd_path,prd_number,content_hash,status,discovered_at,last_seen_at,created_at,updated_at) VALUES('repo','docs/prds/PRD-009.md',9,'hash','pending','before','before','before','before')",[]).unwrap();
        db.conn().execute("INSERT INTO backlog_status_events(event_id,repository_key,prd_path,old_status,new_status,actor,changed_at) VALUES(1,'repo','docs/prds/PRD-009.md','pending','in_progress','system:familiar-ai-run:00001785772020811891-0000057947-000001','before')",[]).unwrap();
        db.conn().execute("INSERT INTO backlog_status_events(event_id,repository_key,prd_path,old_status,new_status,actor,changed_at) VALUES(2,'repo','docs/prds/PRD-009.md','in_progress','pending','ops:alice','before')",[]).unwrap();
        db.conn().execute("INSERT INTO backlog_recovery_events(status_event_id,action,reason) VALUES(2,'release','review was disabled')",[]).unwrap();
        db.conn().execute("INSERT INTO backlog_status_events(event_id,repository_key,prd_path,old_status,new_status,actor,changed_at) VALUES(3,'repo','docs/prds/PRD-009.md','pending','completed','human:alice','before')",[]).unwrap();
        db.conn().execute("INSERT INTO backlog_recovery_events(status_event_id,action,reason) VALUES(3,'manual_complete_override','accepted outside normal review')",[]).unwrap();

        assert_eq!(db.run_migrations().unwrap(), 34);

        let rows: Vec<(i64, String, String)> = {
            let mut stmt = db
                .conn()
                .prepare("SELECT status_event_id,action,reason FROM backlog_recovery_events ORDER BY status_event_id")
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            rows,
            vec![
                (2, "release".into(), "review was disabled".into()),
                (
                    3,
                    "manual_complete_override".into(),
                    "accepted outside normal review".into()
                ),
            ]
        );

        db.conn()
            .execute(
                "INSERT INTO backlog_status_events(event_id,repository_key,prd_path,old_status,new_status,actor,changed_at) VALUES(4,'repo','docs/prds/PRD-009.md','pending','completed','human:bob','before')",
                [],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO backlog_recovery_events(status_event_id,action,reason) VALUES(4,'recorded_complete','merged before tracking existed')",
                [],
            )
            .unwrap();
        let widened: (i64, String, String) = db
            .conn()
            .query_row(
                "SELECT status_event_id,action,reason FROM backlog_recovery_events WHERE status_event_id=4",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            widened,
            (
                4,
                "recorded_complete".into(),
                "merged before tracking existed".into()
            )
        );

        // The rebuilt table's foreign key to backlog_status_events is still enforced.
        let orphan = db.conn().execute(
            "INSERT INTO backlog_recovery_events(status_event_id,action,reason) VALUES(999,'release','orphan')",
            [],
        );
        assert!(orphan.is_err());
    }
}
