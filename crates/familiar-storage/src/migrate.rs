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
    }

    #[test]
    fn migration_is_idempotent() {
        let db = crate::Database::open_in_memory().unwrap();
        let first = db.run_migrations().unwrap();
        let second = db.run_migrations().unwrap();
        assert_eq!(first, 2);
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
        assert_eq!(versions, vec![1, 2]);
    }
}
