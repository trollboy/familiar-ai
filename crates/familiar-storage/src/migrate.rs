use rusqlite::Connection;

use familiar_core::FamiliarError;

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
];

pub fn run_migrations(conn: &Connection) -> familiar_core::Result<usize> {
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
    }

    #[test]
    fn migration_is_idempotent() {
        let db = crate::Database::open_in_memory().unwrap();
        let first = db.run_migrations().unwrap();
        let second = db.run_migrations().unwrap();
        assert_eq!(first, 5);
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
        assert_eq!(versions, vec![1, 2, 3, 4, 5]);
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

        assert_eq!(db.run_migrations().unwrap(), 3);
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
}
