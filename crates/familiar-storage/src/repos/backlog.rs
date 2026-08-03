use familiar_core::{
    BacklogEntry, BacklogStatus, BacklogStatusStore, BacklogStoreError, DiscoveredPrd, PrdId,
    RepositoryIdentity, RepositoryPath,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

pub struct SqliteBacklogRepository<'a> {
    connection: &'a mut Connection,
}

impl<'a> SqliteBacklogRepository<'a> {
    pub fn new(connection: &'a mut Connection) -> Self {
        Self { connection }
    }
}

fn storage(error: rusqlite::Error) -> BacklogStoreError {
    BacklogStoreError::Storage(error.to_string())
}

fn read_entry(
    tx: &Transaction<'_>,
    repository_key: &str,
    prd: &DiscoveredPrd,
) -> Result<BacklogEntry, BacklogStoreError> {
    let status: String = tx.query_row(
        "SELECT status FROM backlog_prds WHERE repository_key = ?1 AND prd_path = ?2 AND missing_since IS NULL",
        params![repository_key, prd.path.as_str()], |row| row.get(0),
    ).map_err(storage)?;
    Ok(BacklogEntry {
        prd: prd.clone(),
        status: BacklogStatus::parse(&status)?,
    })
}

impl BacklogStatusStore for SqliteBacklogRepository<'_> {
    fn reconcile_and_snapshot(
        &mut self,
        repository: &RepositoryIdentity,
        discovered: &[DiscoveredPrd],
    ) -> Result<Vec<BacklogEntry>, BacklogStoreError> {
        let tx = self.connection.transaction().map_err(storage)?;
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE backlog_prds SET missing_since = COALESCE(missing_since, ?2), updated_at = CASE WHEN missing_since IS NULL THEN ?2 ELSE updated_at END WHERE repository_key = ?1",
            params![repository.key, now],
        ).map_err(storage)?;
        for prd in discovered {
            tx.execute(
                "INSERT INTO backlog_prds (repository_key, prd_path, prd_number, content_hash, status, discovered_at, last_seen_at, missing_since, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5, NULL, ?5, ?5)
                 ON CONFLICT(repository_key, prd_path) DO UPDATE SET
                    prd_number = excluded.prd_number,
                    content_hash = excluded.content_hash,
                    last_seen_at = excluded.last_seen_at,
                    missing_since = NULL",
                params![repository.key, prd.path.as_str(), prd.number.to_string(), prd.content_hash, now],
            ).map_err(storage)?;
        }
        let snapshot = discovered
            .iter()
            .map(|prd| read_entry(&tx, &repository.key, prd))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().map_err(storage)?;
        Ok(snapshot)
    }

    fn transition(
        &mut self,
        repository: &RepositoryIdentity,
        path: &RepositoryPath,
        expected: BacklogStatus,
        next: BacklogStatus,
        actor: &str,
    ) -> Result<BacklogEntry, BacklogStoreError> {
        if actor.trim().is_empty() {
            return Err(BacklogStoreError::EmptyActor);
        }
        let tx = self.connection.transaction().map_err(storage)?;
        let row: Option<(u64, String, String)> = tx.query_row(
            "SELECT prd_number, content_hash, status FROM backlog_prds WHERE repository_key = ?1 AND prd_path = ?2",
            params![repository.key, path.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional().map_err(storage)?;
        let (number, content_hash, current_text) =
            row.ok_or_else(|| BacklogStoreError::NotFound(path.clone()))?;
        let current = BacklogStatus::parse(&current_text)?;
        if current != expected {
            return Err(BacklogStoreError::Conflict {
                path: path.clone(),
                expected: expected.as_str(),
                actual: current.as_str(),
            });
        }
        if current != next {
            let now = chrono::Utc::now().to_rfc3339();
            let changed = tx.execute(
                "UPDATE backlog_prds SET status = ?3, updated_at = ?4 WHERE repository_key = ?1 AND prd_path = ?2 AND status = ?5",
                params![repository.key, path.as_str(), next.as_str(), now, expected.as_str()],
            ).map_err(storage)?;
            if changed != 1 {
                return Err(BacklogStoreError::Conflict {
                    path: path.clone(),
                    expected: expected.as_str(),
                    actual: current.as_str(),
                });
            }
            tx.execute(
                "INSERT INTO backlog_status_events (repository_key, prd_path, old_status, new_status, actor, changed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![repository.key, path.as_str(), current.as_str(), next.as_str(), actor, now],
            ).map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        Ok(BacklogEntry {
            prd: DiscoveredPrd {
                id: PrdId::new(number),
                number,
                path: path.clone(),
                title: String::new(),
                dependencies: Vec::new(),
                content_hash,
            },
            status: next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn prd() -> DiscoveredPrd {
        DiscoveredPrd {
            id: PrdId::new(9),
            number: 9,
            path: RepositoryPath::new("docs/prds/PRD-009.md").unwrap(),
            title: "Nine".into(),
            dependencies: vec![],
            content_hash: "abc".into(),
        }
    }
    fn repo() -> RepositoryIdentity {
        RepositoryIdentity {
            worktree: "/tmp/work".into(),
            key: "/tmp/repo/.git".into(),
        }
    }
    #[test]
    fn reconcile_preserves_status_and_transitions_are_checked() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        assert_eq!(
            storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap()[0].status,
            BacklogStatus::Pending
        );
        storage
            .transition(
                &repo(),
                &prd().path,
                BacklogStatus::Pending,
                BacklogStatus::Completed,
                "test",
            )
            .unwrap();
        assert_eq!(
            storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap()[0].status,
            BacklogStatus::Completed
        );
        assert!(matches!(
            storage.transition(
                &repo(),
                &prd().path,
                BacklogStatus::Pending,
                BacklogStatus::Blocked,
                "test"
            ),
            Err(BacklogStoreError::Conflict { .. })
        ));
        storage
            .transition(
                &repo(),
                &prd().path,
                BacklogStatus::Completed,
                BacklogStatus::Completed,
                "test",
            )
            .unwrap();
        let events: i64 = storage
            .connection
            .query_row("SELECT count(*) FROM backlog_status_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(events, 1);
    }
    #[test]
    fn missing_and_reappearing_entry_retains_status() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        storage
            .transition(
                &repo(),
                &prd().path,
                BacklogStatus::Pending,
                BacklogStatus::Blocked,
                "test",
            )
            .unwrap();
        assert!(storage
            .reconcile_and_snapshot(&repo(), &[])
            .unwrap()
            .is_empty());
        assert_eq!(
            storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap()[0].status,
            BacklogStatus::Blocked
        );
    }

    #[test]
    fn transition_rejects_empty_actor_and_isolates_repositories() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let mut storage = SqliteBacklogRepository::new(db.conn_mut());
        storage.reconcile_and_snapshot(&repo(), &[prd()]).unwrap();
        assert!(matches!(
            storage.transition(
                &repo(),
                &prd().path,
                BacklogStatus::Pending,
                BacklogStatus::Completed,
                "  "
            ),
            Err(BacklogStoreError::EmptyActor)
        ));
        let other = RepositoryIdentity {
            worktree: "/tmp/other".into(),
            key: "/tmp/other/.git".into(),
        };
        assert!(matches!(
            storage.transition(
                &other,
                &prd().path,
                BacklogStatus::Pending,
                BacklogStatus::Completed,
                "test"
            ),
            Err(BacklogStoreError::NotFound(_))
        ));
    }
}
