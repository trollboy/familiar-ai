use familiar_ai_core::{
    validate_dependency_closure, BacklogStatus, BootstrapApplied, BootstrapApplyResult,
    BootstrapError, BootstrapManifest, BootstrapRollbackResult, BootstrapStatusReport,
    DiscoveredPrd, PrdId, RepositoryIdentity, BOOTSTRAP_ACTOR, BOOTSTRAP_MANIFEST_PATH,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct SqliteBootstrapRepository<'a> {
    connection: &'a mut Connection,
}
impl<'a> SqliteBootstrapRepository<'a> {
    pub fn new(connection: &'a mut Connection) -> Self {
        Self { connection }
    }
}

fn err(e: rusqlite::Error) -> BootstrapError {
    BootstrapError::Storage(e.to_string())
}
static IDS: AtomicU64 = AtomicU64::new(1);
fn run_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        IDS.fetch_add(1, Ordering::Relaxed)
    )
}

impl SqliteBootstrapRepository<'_> {
    pub fn apply(
        &mut self,
        repository: &RepositoryIdentity,
        discovered: &[DiscoveredPrd],
        manifest: Option<&BootstrapManifest>,
    ) -> Result<BootstrapApplyResult, BootstrapError> {
        let Some(manifest) = manifest else {
            self.validate_all_runs(&repository.key)?;
            return Ok(BootstrapApplyResult::Absent);
        };
        self.validate_all_runs(&repository.key)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let runs: Vec<(String, String, String, Option<String>)> = {
            let mut stmt=tx.prepare("SELECT run_id, canonical_manifest_hash, status, rollback_run_id FROM backlog_bootstrap_runs WHERE repository_key=?1 ORDER BY created_at, run_id").map_err(err)?;
            let rows = stmt
                .query_map([&repository.key], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .map_err(err)?
                .collect::<Result<_, _>>()
                .map_err(err)?;
            rows
        };
        if let Some((_, hash, _, _)) = runs.iter().find(|(_, _, s, _)| s == "applied") {
            if hash == &manifest.canonical_hash {
                return Ok(BootstrapApplyResult::AlreadyApplied);
            }
            return Err(BootstrapError::Conflict("BootstrapManifestChanged: an applied lineage has a different canonical manifest hash".into()));
        }
        let reapply = if runs.is_empty() {
            None
        } else if runs.len() == 1
            && runs[0].2 == "rolled_back"
            && runs[0].1 == manifest.canonical_hash
            && runs[0].3.is_some()
        {
            Some(runs[0].0.clone())
        } else {
            return Err(BootstrapError::Conflict(
                "a prior bootstrap lineage disallows application".into(),
            ));
        };
        let mut statuses = BTreeMap::new();
        let mut evidenced = BTreeSet::new();
        for prd in discovered {
            let row: Option<(String,Option<String>,i64)> = tx.query_row("SELECT status, missing_since, (SELECT count(*) FROM backlog_status_events e WHERE e.repository_key=p.repository_key AND e.prd_path=p.prd_path) FROM backlog_prds p WHERE repository_key=?1 AND prd_path=?2", params![repository.key,prd.path.as_str()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(err)?;
            let Some((status, missing, event_count)) = row else {
                return Err(BootstrapError::Conflict(format!(
                    "missing target row {}",
                    prd.path
                )));
            };
            let parsed = BacklogStatus::parse(&status)
                .map_err(|e| BootstrapError::Storage(e.to_string()))?;
            statuses.insert(prd.id.clone(), parsed);
            if parsed == BacklogStatus::Completed && event_count > 0 {
                evidenced.insert(prd.id.clone());
            }
            if manifest.items.iter().any(|i| i.path == prd.path) {
                if missing.is_some() {
                    return Err(BootstrapError::Ineligible(format!(
                        "{} is marked missing",
                        prd.path
                    )));
                }
                if parsed != BacklogStatus::Pending {
                    return Err(BootstrapError::Ineligible(format!(
                        "{} has status {}",
                        prd.path,
                        parsed.as_str()
                    )));
                }
                if event_count != 0 && reapply.is_none() {
                    return Err(BootstrapError::Ineligible(format!(
                        "{} has prior status events",
                        prd.path
                    )));
                }
                if reapply.is_some() && event_count != 2 {
                    return Err(BootstrapError::Ineligible(format!(
                        "{} has later or incomplete rollback events",
                        prd.path
                    )));
                }
            }
        }
        validate_dependency_closure(manifest, discovered, &statuses, &evidenced)?;
        let id = run_id("bootstrap");
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute("INSERT INTO backlog_bootstrap_runs (run_id,repository_key,canonical_manifest_hash,raw_manifest_hash,manifest_path,manifest_version,status,item_count,applied_at,reapplies_run_id,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,'applied',?7,?8,?9,?8,?8)",params![id,repository.key,manifest.canonical_hash,manifest.raw_hash,BOOTSTRAP_MANIFEST_PATH,manifest.version,manifest.items.len(),now,reapply]).map_err(err)?;
        for (index, item) in manifest.items.iter().enumerate() {
            let changed=tx.execute("UPDATE backlog_prds SET status='completed',updated_at=?3 WHERE repository_key=?1 AND prd_path=?2 AND status='pending' AND missing_since IS NULL AND content_hash=?4",params![repository.key,item.path.as_str(),now,item.observed_content_hash]).map_err(err)?;
            if changed != 1 {
                return Err(BootstrapError::Conflict(format!(
                    "optimistic status write failed for {}",
                    item.path
                )));
            }
            tx.execute("INSERT INTO backlog_status_events(repository_key,prd_path,old_status,new_status,actor,changed_at) VALUES(?1,?2,'pending','completed',?3,?4)",params![repository.key,item.path.as_str(),BOOTSTRAP_ACTOR,now]).map_err(err)?;
            let event = tx.last_insert_rowid();
            tx.execute("INSERT INTO backlog_bootstrap_items(run_id,ordinal,repository_key,prd_path,prd_number,prd_suffix,declared_content_hash,observed_content_hash,old_status,new_status,status_event_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending','completed',?9)",params![id,index+1,repository.key,item.path.as_str(),item.prd_number,item.prd_suffix.map(|c| c.to_string()),item.declared_content_hash,item.observed_content_hash,event]).map_err(err)?;
        }
        tx.commit().map_err(err)?;
        Ok(BootstrapApplyResult::Applied(BootstrapApplied {
            run_id: id,
            item_count: manifest.items.len(),
            canonical_hash: manifest.canonical_hash.clone(),
        }))
    }

    pub fn status(
        &mut self,
        repository: &RepositoryIdentity,
        manifest: Option<&BootstrapManifest>,
    ) -> Result<BootstrapStatusReport, BootstrapError> {
        self.validate_all_runs(&repository.key)?;
        let row: Option<(String,String,String,i64)> = self.connection.query_row("SELECT run_id,canonical_manifest_hash,status,item_count FROM backlog_bootstrap_runs WHERE repository_key=?1 ORDER BY created_at DESC,run_id DESC LIMIT 1",[&repository.key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(err)?;
        if let Some((id, hash, state, count)) = row {
            return Ok(BootstrapStatusReport {
                state,
                repository_key: repository.key.clone(),
                run_id: Some(id),
                canonical_hash: Some(hash),
                item_count: count as usize,
            });
        }
        Ok(BootstrapStatusReport {
            state: if manifest.is_some() {
                "eligible"
            } else {
                "absent"
            }
            .into(),
            repository_key: repository.key.clone(),
            run_id: None,
            canonical_hash: manifest.map(|m| m.canonical_hash.clone()),
            item_count: manifest.map_or(0, |m| m.items.len()),
        })
    }

    pub fn rollback(
        &mut self,
        repository: &RepositoryIdentity,
        discovered: &[DiscoveredPrd],
        target: &str,
        actor: &str,
        reason: &str,
    ) -> Result<BootstrapRollbackResult, BootstrapError> {
        if actor.trim().is_empty() || reason.trim().is_empty() {
            return Err(BootstrapError::RollbackIneligible(
                "actor and reason must be non-empty".into(),
            ));
        }
        self.validate_all_runs(&repository.key)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let run:Option<(String,String,Option<String>,i64)>=tx.query_row("SELECT repository_key,status,rollback_run_id,item_count FROM backlog_bootstrap_runs WHERE run_id=?1",[target],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(err)?;
        let Some((key, status, rollback, count)) = run else {
            return Err(BootstrapError::RollbackIneligible(
                "bootstrap run not found".into(),
            ));
        };
        if key != repository.key {
            return Err(BootstrapError::RollbackIneligible(
                "bootstrap run belongs to another repository".into(),
            ));
        }
        if status != "applied" || rollback.is_some() {
            return Err(BootstrapError::RollbackIneligible(
                "bootstrap run is not an unrolled-back applied run".into(),
            ));
        }
        let mut items: Vec<(i64, String, String, i64)> = {
            let mut s=tx.prepare("SELECT ordinal,prd_path,observed_content_hash,status_event_id FROM backlog_bootstrap_items WHERE run_id=?1 ORDER BY ordinal").map_err(err)?;
            let rows = s
                .query_map([target], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .map_err(err)?
                .collect::<Result<_, _>>()
                .map_err(err)?;
            rows
        };
        if items.len() != count as usize {
            return Err(BootstrapError::AuditCorrupt(
                "run item count mismatch".into(),
            ));
        }
        let discovered_by_path: BTreeMap<_, _> =
            discovered.iter().map(|p| (p.path.as_str(), p)).collect();
        let targets: BTreeSet<_> = items.iter().map(|i| i.1.as_str()).collect();
        let mut blockers = Vec::new();
        for (_, path, hash, event) in &items {
            let Some(prd) = discovered_by_path.get(path.as_str()) else {
                blockers.push(format!("{path}: PRD missing"));
                continue;
            };
            if &prd.content_hash != hash {
                blockers.push(format!("{path}: content changed"));
            }
            let current:(String,i64)=tx.query_row("SELECT status,(SELECT max(event_id) FROM backlog_status_events WHERE repository_key=?1 AND prd_path=?2) FROM backlog_prds WHERE repository_key=?1 AND prd_path=?2",params![repository.key,path],|r|Ok((r.get(0)?,r.get(1)?))).map_err(err)?;
            if current.0 != "completed" || current.1 != *event {
                blockers.push(format!("{path}: status or latest event changed"));
            }
        }
        for prd in discovered {
            if targets.contains(prd.path.as_str()) {
                continue;
            }
            let status: String = tx
                .query_row(
                    "SELECT status FROM backlog_prds WHERE repository_key=?1 AND prd_path=?2",
                    params![repository.key, prd.path.as_str()],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if (status == "in_progress" || status == "completed")
                && depends_on_target(prd, discovered, &targets, &mut BTreeSet::new())
            {
                blockers.push(format!("{}: dependent is {status}", prd.path));
            }
        }
        if !blockers.is_empty() {
            blockers.sort();
            return Err(BootstrapError::RollbackIneligible(blockers.join("; ")));
        }
        let rollback_id = run_id("rollback");
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute("INSERT INTO backlog_bootstrap_rollbacks(rollback_run_id,bootstrap_run_id,repository_key,actor,reason,item_count,created_at,completed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",params![rollback_id,target,repository.key,actor,reason,count,now]).map_err(err)?;
        items.reverse();
        for (out_index, (_, path, _, _)) in items.iter().enumerate() {
            let changed=tx.execute("UPDATE backlog_prds SET status='pending',updated_at=?3 WHERE repository_key=?1 AND prd_path=?2 AND status='completed'",params![repository.key,path,now]).map_err(err)?;
            if changed != 1 {
                return Err(BootstrapError::Conflict(format!(
                    "rollback write conflict for {path}"
                )));
            }
            tx.execute("INSERT INTO backlog_status_events(repository_key,prd_path,old_status,new_status,actor,changed_at)VALUES(?1,?2,'completed','pending',?3,?4)",params![repository.key,path,actor,now]).map_err(err)?;
            let event = tx.last_insert_rowid();
            tx.execute("INSERT INTO backlog_bootstrap_rollback_items(rollback_run_id,ordinal,prd_path,old_status,restored_status,status_event_id)VALUES(?1,?2,?3,'completed','pending',?4)",params![rollback_id,out_index+1,path,event]).map_err(err)?;
        }
        tx.execute("UPDATE backlog_bootstrap_runs SET status='rolled_back',rolled_back_at=?2,rollback_run_id=?3,updated_at=?2 WHERE run_id=?1",params![target,now,rollback_id]).map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(BootstrapRollbackResult {
            rollback_run_id: rollback_id,
            item_count: count as usize,
        })
    }

    fn validate_all_runs(&self, key: &str) -> Result<(), BootstrapError> {
        let corrupt:i64=self.connection.query_row("SELECT count(*) FROM backlog_bootstrap_runs r WHERE repository_key=?1 AND ((SELECT count(*) FROM backlog_bootstrap_items i WHERE i.run_id=r.run_id) != r.item_count OR EXISTS(SELECT 1 FROM backlog_bootstrap_items i LEFT JOIN backlog_status_events e ON e.event_id=i.status_event_id WHERE i.run_id=r.run_id AND (e.event_id IS NULL OR e.repository_key!=i.repository_key OR e.prd_path!=i.prd_path OR e.old_status!=i.old_status OR e.new_status!=i.new_status OR e.actor!=?2)))",params![key,BOOTSTRAP_ACTOR],|r|r.get(0)).map_err(err)?;
        if corrupt > 0 {
            Err(BootstrapError::AuditCorrupt(
                "bootstrap run/item/event linkage is inconsistent".into(),
            ))
        } else {
            Ok(())
        }
    }
}

fn depends_on_target(
    prd: &DiscoveredPrd,
    all: &[DiscoveredPrd],
    targets: &BTreeSet<&str>,
    visiting: &mut BTreeSet<PrdId>,
) -> bool {
    if !visiting.insert(prd.id.clone()) {
        return false;
    }
    for dep in &prd.dependencies {
        if let Some(p) = all.iter().find(|p| &p.id == dep) {
            if targets.contains(p.path.as_str()) || depends_on_target(p, all, targets, visiting) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, SqliteBacklogRepository};
    use familiar_ai_core::{BacklogStatusStore, BootstrapItem, RepositoryPath};

    fn repo() -> RepositoryIdentity {
        RepositoryIdentity {
            worktree: "/tmp/bootstrap".into(),
            key: "bootstrap-repo".into(),
        }
    }
    fn prd(n: u64) -> DiscoveredPrd {
        DiscoveredPrd {
            id: PrdId::new(n),
            number: n,
            path: RepositoryPath::new(format!("docs/prds/PRD-{n:03}.md")).unwrap(),
            location: familiar_ai_core::PrdLocation::Active,
            title: n.to_string(),
            dependencies: vec![],
            content_hash: format!("{n:064x}"),
        }
    }
    fn manifest(prds: &[DiscoveredPrd]) -> BootstrapManifest {
        BootstrapManifest {
            version: 1,
            canonical_hash: "a".repeat(64),
            raw_hash: "b".repeat(64),
            items: prds
                .iter()
                .map(|p| BootstrapItem {
                    path: p.path.clone(),
                    prd_number: p.number,
                    prd_suffix: p.id.suffix(),
                    declared_content_hash: p.content_hash.clone(),
                    observed_content_hash: p.content_hash.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn apply_is_atomic_idempotent_and_rollback_is_append_only() {
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let prds = vec![prd(1), prd(2)];
        SqliteBacklogRepository::new(db.conn_mut())
            .reconcile_and_snapshot(&repo(), &prds)
            .unwrap();
        let m = manifest(&prds);
        let applied = SqliteBootstrapRepository::new(db.conn_mut())
            .apply(&repo(), &prds, Some(&m))
            .unwrap();
        let run = match applied {
            BootstrapApplyResult::Applied(v) => v,
            _ => panic!(),
        };
        assert_eq!(
            SqliteBootstrapRepository::new(db.conn_mut())
                .apply(&repo(), &prds, Some(&m))
                .unwrap(),
            BootstrapApplyResult::AlreadyApplied
        );
        let counts:(i64,i64)=db.conn().query_row("SELECT (SELECT count(*) FROM backlog_bootstrap_items),(SELECT count(*) FROM backlog_status_events)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(counts, (2, 2));
        let rolled = SqliteBootstrapRepository::new(db.conn_mut())
            .rollback(&repo(), &prds, &run.run_id, "human:test", "correction")
            .unwrap();
        assert_eq!(rolled.item_count, 2);
        let statuses: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM backlog_prds WHERE status='pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(statuses, 2);
        let events: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM backlog_status_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(events, 4);
    }
}
