use familiar_core::{models::NewFileSummary, CanonicalFileIdentity, FamiliarError};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::{repos::now_rfc3339, Database};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleChange {
    Create,
    Modify,
}
impl LifecycleChange {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Modify => "modify",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetirementReason {
    Deleted,
    Modified,
    Renamed,
    Ineligible,
}
impl RetirementReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deleted => "deleted",
            Self::Modified => "modified",
            Self::Renamed => "renamed",
            Self::Ineligible => "ineligible",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSummaryWork {
    pub id: i64,
    pub project_id: i64,
    pub path: String,
    pub kind: String,
    pub status: String,
    pub attempt_count: i64,
    pub observation_order: i64,
    pub dispatch_deferred: bool,
    pub source_mtime: Option<i64>,
    pub source_size: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleOutcome {
    pub observation_order: i64,
    pub pending_id: Option<i64>,
    pub tombstone_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanStatus {
    pub id: i64,
    pub project_id: i64,
    pub status: String,
    pub enumeration_status: String,
    pub reconciliation_status: String,
    pub visited: i64,
    pub eligible: i64,
    pub excluded: i64,
    pub rejected: i64,
    pub failed: i64,
    pub staged: i64,
    pub absence_permitted: bool,
    pub pending_summaries: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRun {
    pub id: i64,
    pub project_id: i64,
    pub root: String,
    pub start_observation_order: i64,
}

pub trait LifecycleRepository {
    fn observe_change(
        &self,
        project_id: i64,
        path: &str,
        change: LifecycleChange,
        source: &str,
        deferred: bool,
    ) -> familiar_core::Result<LifecycleOutcome>;
    fn retire_absent(
        &self,
        project_id: i64,
        path: &str,
        reason: RetirementReason,
        source: &str,
        cause_id: Option<i64>,
    ) -> familiar_core::Result<LifecycleOutcome>;
    fn observe_exact_rename(
        &self,
        project_id: i64,
        old_path: &str,
        new_path: &str,
        source: &str,
        deferred: bool,
    ) -> familiar_core::Result<LifecycleOutcome>;
    fn list_pending_summary_work(
        &self,
        project_id: i64,
        limit: usize,
    ) -> familiar_core::Result<Vec<PendingSummaryWork>>;
    fn complete_summary_work(&self, project_id: i64, path: &str) -> familiar_core::Result<()>;
    fn commit_summary_work(&self, summary: &NewFileSummary) -> familiar_core::Result<()>;
    fn fail_summary_work(
        &self,
        project_id: i64,
        path: &str,
        error: &str,
    ) -> familiar_core::Result<()>;
    fn latest_scan_status(&self, project_id: i64) -> familiar_core::Result<Option<ScanStatus>>;
}

fn db_error(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}
fn validate(project_id: i64, path: &str) -> familiar_core::Result<()> {
    CanonicalFileIdentity::validate_stored(project_id, path)
        .map(|_| ())
        .map_err(|e| FamiliarError::Database(format!("invalid canonical lifecycle identity: {e}")))
}
fn next_order(tx: &Transaction<'_>, project_id: i64) -> familiar_core::Result<i64> {
    tx.execute("INSERT INTO repository_observation_orders(project_id,last_order) VALUES(?1,1) ON CONFLICT(project_id) DO UPDATE SET last_order=last_order+1", [project_id]).map_err(db_error)?;
    tx.query_row(
        "SELECT last_order FROM repository_observation_orders WHERE project_id=?1",
        [project_id],
        |r| r.get(0),
    )
    .map_err(db_error)
}
fn ensure_project_ready(tx: &Transaction<'_>, project_id: i64) -> familiar_core::Result<()> {
    let active: Option<i64> = tx
        .query_row(
            "SELECT active FROM projects WHERE id=?1",
            [project_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if active != Some(1) {
        return Err(FamiliarError::Database(format!(
            "project {project_id} is missing or inactive"
        )));
    }
    let unresolved: i64 = tx.query_row("SELECT COUNT(*) FROM file_summary_reconciliation_records WHERE project_id=?1 AND classification='unresolved' AND rolled_back_at IS NULL", [project_id], |r| r.get(0)).map_err(db_error)?;
    if unresolved != 0 {
        return Err(FamiliarError::Database(format!(
            "project {project_id} has unresolved legacy identities"
        )));
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn preserve_and_delete(
    tx: &Transaction<'_>,
    project_id: i64,
    path: &str,
    reason: RetirementReason,
    source: &str,
    cause_id: Option<i64>,
    related: Option<&str>,
    order: i64,
) -> familiar_core::Result<Option<i64>> {
    let now = now_rfc3339();
    tx.execute("INSERT OR IGNORE INTO file_summary_lifecycle_tombstones(project_id,original_file_summary_id,path,summary,tags_json,extracted_symbols_json,last_known_mtime,last_known_size,last_updated_at,original_created_at,original_updated_at,reason,cause_source,cause_id,related_path,observation_order,retired_at) SELECT project_id,id,path,summary,tags_json,extracted_symbols_json,last_known_mtime,last_known_size,last_updated_at,created_at,updated_at,?3,?4,?5,?6,?7,?8 FROM file_summaries WHERE project_id=?1 AND path=?2", params![project_id,path,reason.as_str(),source,cause_id,related,order,now]).map_err(db_error)?;
    let tombstone = tx.query_row("SELECT id FROM file_summary_lifecycle_tombstones WHERE project_id=?1 AND path=?2 AND observation_order=?3", params![project_id,path,order], |r| r.get(0)).optional().map_err(db_error)?;
    tx.execute(
        "DELETE FROM file_summaries WHERE project_id=?1 AND path=?2",
        params![project_id, path],
    )
    .map_err(db_error)?;
    Ok(tombstone)
}
fn upsert_pending(
    tx: &Transaction<'_>,
    project_id: i64,
    path: &str,
    kind: LifecycleChange,
    order: i64,
    deferred: bool,
) -> familiar_core::Result<i64> {
    let now = now_rfc3339();
    tx.execute("INSERT INTO pending_summary_work(project_id,path,kind,status,observation_order,dispatch_deferred,created_at,updated_at) VALUES(?1,?2,?3,'pending',?4,?5,?6,?6) ON CONFLICT(project_id,path) DO UPDATE SET kind=excluded.kind,status='pending',observation_order=excluded.observation_order,dispatch_deferred=excluded.dispatch_deferred,last_error=NULL,updated_at=excluded.updated_at,completed_at=NULL", params![project_id,path,kind.as_str(),order,deferred as i64,now]).map_err(db_error)?;
    tx.query_row(
        "SELECT id FROM pending_summary_work WHERE project_id=?1 AND path=?2",
        params![project_id, path],
        |r| r.get(0),
    )
    .map_err(db_error)
}

impl LifecycleRepository for Database {
    fn observe_change(
        &self,
        project_id: i64,
        path: &str,
        change: LifecycleChange,
        source: &str,
        deferred: bool,
    ) -> familiar_core::Result<LifecycleOutcome> {
        validate(project_id, path)?;
        let tx = self.conn().unchecked_transaction().map_err(db_error)?;
        ensure_project_ready(&tx, project_id)?;
        let order = next_order(&tx, project_id)?;
        let tombstone = if change == LifecycleChange::Modify {
            preserve_and_delete(
                &tx,
                project_id,
                path,
                RetirementReason::Modified,
                source,
                None,
                None,
                order,
            )?
        } else {
            None
        };
        let pending = upsert_pending(&tx, project_id, path, change, order, deferred)?;
        tx.execute("INSERT INTO repository_observations(project_id,observation_order,path,kind,source,outcome,recorded_at) VALUES(?1,?2,?3,?4,?5,'pending',?6)",params![project_id,order,path,if change==LifecycleChange::Create{"created"}else{"modified"},source,now_rfc3339()]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(LifecycleOutcome {
            observation_order: order,
            pending_id: Some(pending),
            tombstone_id: tombstone,
        })
    }
    fn retire_absent(
        &self,
        project_id: i64,
        path: &str,
        reason: RetirementReason,
        source: &str,
        cause_id: Option<i64>,
    ) -> familiar_core::Result<LifecycleOutcome> {
        validate(project_id, path)?;
        let tx = self.conn().unchecked_transaction().map_err(db_error)?;
        ensure_project_ready(&tx, project_id)?;
        let order = next_order(&tx, project_id)?;
        let tombstone =
            preserve_and_delete(&tx, project_id, path, reason, source, cause_id, None, order)?;
        tx.execute("UPDATE pending_summary_work SET status='superseded',updated_at=?3 WHERE project_id=?1 AND path=?2 AND status IN ('pending','leased','failed','interrupted')",params![project_id,path,now_rfc3339()]).map_err(db_error)?;
        tx.execute("INSERT INTO repository_observations(project_id,observation_order,path,kind,source,outcome,recorded_at) VALUES(?1,?2,?3,'removed',?4,'retired',?5)",params![project_id,order,path,source,now_rfc3339()]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(LifecycleOutcome {
            observation_order: order,
            pending_id: None,
            tombstone_id: tombstone,
        })
    }
    fn observe_exact_rename(
        &self,
        project_id: i64,
        old_path: &str,
        new_path: &str,
        source: &str,
        deferred: bool,
    ) -> familiar_core::Result<LifecycleOutcome> {
        validate(project_id, old_path)?;
        validate(project_id, new_path)?;
        if old_path == new_path {
            return Err(FamiliarError::Database(
                "case-only or identical rename is ambiguous".into(),
            ));
        }
        let tx = self.conn().unchecked_transaction().map_err(db_error)?;
        ensure_project_ready(&tx, project_id)?;
        let order = next_order(&tx, project_id)?;
        let target_exists: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM file_summaries WHERE project_id=?1 AND path=?2",
                params![project_id, new_path],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if target_exists != 0 {
            return Err(FamiliarError::Database(
                "rename target already has an active summary".into(),
            ));
        }
        let tombstone = preserve_and_delete(
            &tx,
            project_id,
            old_path,
            RetirementReason::Renamed,
            source,
            None,
            Some(new_path),
            order,
        )?;
        tx.execute("UPDATE pending_summary_work SET status='superseded',updated_at=?3 WHERE project_id=?1 AND path=?2",params![project_id,old_path,now_rfc3339()]).map_err(db_error)?;
        let pending = upsert_pending(
            &tx,
            project_id,
            new_path,
            LifecycleChange::Create,
            order,
            deferred,
        )?;
        for (path, kind, related) in [
            (old_path, "renamed_from", new_path),
            (new_path, "renamed_to", old_path),
        ] {
            tx.execute("INSERT INTO repository_observations(project_id,observation_order,path,kind,source,related_path,outcome,recorded_at) VALUES(?1,?2,?3,?4,?5,?6,'pending',?7)",params![project_id,if kind=="renamed_from"{order}else{next_order(&tx,project_id)?},path,kind,source,related,now_rfc3339()]).map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        Ok(LifecycleOutcome {
            observation_order: order,
            pending_id: Some(pending),
            tombstone_id: tombstone,
        })
    }
    fn list_pending_summary_work(
        &self,
        project_id: i64,
        limit: usize,
    ) -> familiar_core::Result<Vec<PendingSummaryWork>> {
        let mut stmt=self.conn().prepare("SELECT id,project_id,path,kind,status,attempt_count,observation_order,dispatch_deferred,source_mtime,source_size FROM pending_summary_work WHERE project_id=?1 AND status IN ('pending','interrupted','failed') ORDER BY observation_order,path LIMIT ?2").map_err(db_error)?;
        let rows = stmt
            .query_map(params![project_id, limit as i64], |r| {
                Ok(PendingSummaryWork {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    path: r.get(2)?,
                    kind: r.get(3)?,
                    status: r.get(4)?,
                    attempt_count: r.get(5)?,
                    observation_order: r.get(6)?,
                    dispatch_deferred: r.get::<_, i64>(7)? != 0,
                    source_mtime: r.get(8)?,
                    source_size: r.get(9)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }
    fn complete_summary_work(&self, project_id: i64, path: &str) -> familiar_core::Result<()> {
        validate(project_id, path)?;
        self.conn().execute("UPDATE pending_summary_work SET status='completed',dispatch_deferred=0,completed_at=?3,updated_at=?3 WHERE project_id=?1 AND path=?2",params![project_id,path,now_rfc3339()]).map_err(db_error)?;
        Ok(())
    }
    fn commit_summary_work(&self, summary: &NewFileSummary) -> familiar_core::Result<()> {
        validate(summary.project_id, &summary.path)?;
        let tx = self.conn().unchecked_transaction().map_err(db_error)?;
        let now = now_rfc3339();
        let tags = serde_json::to_string(&summary.tags)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        let symbols = serde_json::to_string(&summary.extracted_symbols)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        tx.execute(
            crate::sql::UPSERT_FILE_SUMMARY,
            params![
                summary.project_id,
                summary.path,
                summary.summary,
                tags,
                symbols,
                summary.last_known_mtime,
                summary.last_known_size,
                now
            ],
        )
        .map_err(db_error)?;
        tx.execute("UPDATE pending_summary_work SET status='completed',dispatch_deferred=0,completed_at=?3,updated_at=?3 WHERE project_id=?1 AND path=?2 AND observation_order=(SELECT MAX(observation_order) FROM pending_summary_work WHERE project_id=?1 AND path=?2)",params![summary.project_id,summary.path,now]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(())
    }
    fn fail_summary_work(
        &self,
        project_id: i64,
        path: &str,
        error: &str,
    ) -> familiar_core::Result<()> {
        self.conn().execute("UPDATE pending_summary_work SET status='failed',attempt_count=attempt_count+1,last_error=?3,updated_at=?4 WHERE project_id=?1 AND path=?2",params![project_id,path,error,now_rfc3339()]).map_err(db_error)?;
        Ok(())
    }
    fn latest_scan_status(&self, project_id: i64) -> familiar_core::Result<Option<ScanStatus>> {
        self.conn().query_row("SELECT r.id,r.project_id,r.status,r.enumeration_status,r.reconciliation_status,r.visited_count,r.eligible_count,r.excluded_count,r.rejected_count,r.failed_count,r.staged_count,r.absence_permitted,(SELECT COUNT(*) FROM pending_summary_work p WHERE p.project_id=r.project_id AND p.status IN ('pending','leased','failed','interrupted')) FROM repository_scan_runs r WHERE r.project_id=?1 ORDER BY r.id DESC LIMIT 1",[project_id],|r|Ok(ScanStatus{id:r.get(0)?,project_id:r.get(1)?,status:r.get(2)?,enumeration_status:r.get(3)?,reconciliation_status:r.get(4)?,visited:r.get(5)?,eligible:r.get(6)?,excluded:r.get(7)?,rejected:r.get(8)?,failed:r.get(9)?,staged:r.get(10)?,absence_permitted:r.get::<_,i64>(11)?!=0,pending_summaries:r.get(12)?})).optional().map_err(db_error)
    }
}

impl Database {
    pub fn start_repository_scan(
        &self,
        project_id: i64,
        root: &str,
        policy_version: &str,
    ) -> familiar_core::Result<ScanRun> {
        let tx = self.conn().unchecked_transaction().map_err(db_error)?;
        ensure_project_ready(&tx, project_id)?;
        let registered: String = tx
            .query_row(
                "SELECT repo_root FROM projects WHERE id=?1",
                [project_id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if registered != root {
            return Err(FamiliarError::Database(
                "scan root does not match registered project root".into(),
            ));
        }
        let now = now_rfc3339();
        tx.execute("UPDATE repository_scan_runs SET status='interrupted',enumeration_status=CASE WHEN enumeration_status='complete' THEN enumeration_status ELSE 'interrupted' END,reconciliation_status=CASE WHEN reconciliation_status='complete' THEN reconciliation_status ELSE 'interrupted' END,updated_at=?2,completed_at=?2 WHERE project_id=?1 AND status IN ('running','enumeration_complete')",params![project_id,now]).map_err(db_error)?;
        let boundary: Option<i64> = tx
            .query_row(
                "SELECT last_order FROM repository_observation_orders WHERE project_id=?1",
                [project_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        tx.execute("INSERT INTO repository_scan_runs(project_id,root,policy_version,start_observation_order,status,enumeration_status,reconciliation_status,started_at,updated_at) VALUES(?1,?2,?3,?4,'running','running','not_started',?5,?5)",params![project_id,root,policy_version,boundary.unwrap_or(0),now]).map_err(db_error)?;
        let id = tx.last_insert_rowid();
        tx.commit().map_err(db_error)?;
        Ok(ScanRun {
            id,
            project_id,
            root: root.into(),
            start_observation_order: boundary.unwrap_or(0),
        })
    }
    pub fn stage_scan_entry(
        &self,
        run: &ScanRun,
        path: &str,
        classification: &str,
        detail: Option<&str>,
    ) -> familiar_core::Result<()> {
        validate(run.project_id, path)?;
        if !matches!(
            classification,
            "eligible" | "excluded" | "rejected" | "failed"
        ) {
            return Err(FamiliarError::Database(
                "invalid scan classification".into(),
            ));
        }
        let changed=self.conn().execute("INSERT OR IGNORE INTO repository_scan_entries(scan_run_id,project_id,path,classification,detail) VALUES(?1,?2,?3,?4,?5)",params![run.id,run.project_id,path,classification,detail]).map_err(db_error)?;
        if changed != 0 {
            let column = match classification {
                "eligible" => "eligible_count",
                "excluded" => "excluded_count",
                "rejected" => "rejected_count",
                _ => "failed_count",
            };
            let sql=format!("UPDATE repository_scan_runs SET visited_count=visited_count+1,staged_count=staged_count+1,{column}={column}+1,progress_path=?2,updated_at=?3 WHERE id=?1 AND status='running'");
            self.conn()
                .execute(&sql, params![run.id, path, now_rfc3339()])
                .map_err(db_error)?;
        }
        Ok(())
    }
    pub fn fail_repository_scan(&self, run_id: i64, error: &str) -> familiar_core::Result<()> {
        self.conn().execute("UPDATE repository_scan_runs SET status='incomplete',enumeration_status='incomplete',reconciliation_status='not_started',absence_permitted=0,error=?2,updated_at=?3,completed_at=?3 WHERE id=?1",params![run_id,error,now_rfc3339()]).map_err(db_error)?;
        Ok(())
    }
    pub fn mark_scan_enumeration_complete(&self, run_id: i64) -> familiar_core::Result<()> {
        self.conn().execute("UPDATE repository_scan_runs SET status='enumeration_complete',enumeration_status='complete',reconciliation_status='running',absence_permitted=1,updated_at=?2 WHERE id=?1 AND status='running'",params![run_id,now_rfc3339()]).map_err(db_error)?;
        Ok(())
    }
    pub fn reconcile_repository_scan(&self, run: &ScanRun) -> familiar_core::Result<()> {
        let state: Option<String> = self
            .conn()
            .query_row(
                "SELECT status FROM repository_scan_runs WHERE id=?1 AND project_id=?2",
                params![run.id, run.project_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if state.as_deref() != Some("enumeration_complete") {
            return Err(FamiliarError::Database(
                "only an enumeration-complete scan may reconcile absence".into(),
            ));
        }
        let staged: Vec<String> = {
            let mut s=self.conn().prepare("SELECT path FROM repository_scan_entries WHERE scan_run_id=?1 AND classification='eligible' ORDER BY path").map_err(db_error)?;
            let rows = s
                .query_map([run.id], |r| r.get(0))
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            rows
        };
        for path in &staged {
            let later:i64=self.conn().query_row("SELECT COUNT(*) FROM repository_observations WHERE project_id=?1 AND path=?2 AND observation_order>?3",params![run.project_id,path,run.start_observation_order],|r|r.get(0)).map_err(db_error)?;
            if later != 0 {
                self.conn().execute("UPDATE repository_scan_runs SET later_watcher_wins_count=later_watcher_wins_count+1 WHERE id=?1",[run.id]).map_err(db_error)?;
                continue;
            }
            let active: i64 = self
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM file_summaries WHERE project_id=?1 AND path=?2",
                    params![run.project_id, path],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if active != 0 {
                self.conn().execute("UPDATE repository_scan_runs SET unchanged_count=unchanged_count+1 WHERE id=?1",[run.id]).map_err(db_error)?;
            } else {
                self.observe_change(run.project_id, path, LifecycleChange::Create, "scan", true)?;
                self.conn().execute("UPDATE repository_scan_runs SET pending_create_count=pending_create_count+1 WHERE id=?1",[run.id]).map_err(db_error)?;
            }
        }
        let active: Vec<String> = {
            let mut s = self
                .conn()
                .prepare("SELECT path FROM file_summaries WHERE project_id=?1 ORDER BY path")
                .map_err(db_error)?;
            let rows = s
                .query_map([run.project_id], |r| r.get(0))
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            rows
        };
        for path in active {
            if staged.binary_search(&path).is_ok() {
                continue;
            }
            let later:i64=self.conn().query_row("SELECT COUNT(*) FROM repository_observations WHERE project_id=?1 AND path=?2 AND observation_order>?3",params![run.project_id,path,run.start_observation_order],|r|r.get(0)).map_err(db_error)?;
            if later != 0 {
                continue;
            }
            let host = std::path::Path::new(&run.root).join(&path);
            match std::fs::symlink_metadata(&host) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.retire_absent(
                        run.project_id,
                        &path,
                        RetirementReason::Deleted,
                        "scan",
                        Some(run.id),
                    )?;
                    self.conn().execute("UPDATE repository_scan_runs SET retired_delete_count=retired_delete_count+1 WHERE id=?1",[run.id]).map_err(db_error)?;
                }
                Ok(meta) if !meta.file_type().is_file() => {
                    self.retire_absent(
                        run.project_id,
                        &path,
                        RetirementReason::Ineligible,
                        "scan",
                        Some(run.id),
                    )?;
                    self.conn().execute("UPDATE repository_scan_runs SET retired_ineligible_count=retired_ineligible_count+1 WHERE id=?1",[run.id]).map_err(db_error)?;
                }
                Ok(_) => {}
                Err(e) => {
                    self.fail_repository_scan(
                        run.id,
                        &format!("absence revalidation failed for {path}: {e}"),
                    )?;
                    return Err(FamiliarError::Database(format!(
                        "absence revalidation failed: {e}"
                    )));
                }
            }
        }
        self.conn().execute("UPDATE repository_scan_runs SET status='reconciliation_complete',reconciliation_status='complete',updated_at=?2,completed_at=?2 WHERE id=?1",params![run.id,now_rfc3339()]).map_err(db_error)?;
        Ok(())
    }
}
