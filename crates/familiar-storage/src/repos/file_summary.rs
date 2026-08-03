use familiar_core::models::{FileSummary, NewFileSummary};
use familiar_core::CanonicalFileIdentity;
use familiar_core::FamiliarError;
use rusqlite::{params, OptionalExtension, Transaction};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use super::{json_to_vec, now_rfc3339, parse_dt, vec_to_json};
use crate::sql;
use crate::Database;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReconciliationReason {
    MissingProject,
    NonAbsoluteNoncanonical,
    RegisteredRootMismatch,
    LexicalOrTraversalFailure,
    EmptyRelativeIdentity,
    LosslessRepresentationFailure,
    UnsupportedHostPathForm,
    InternalPersistenceOrValidationFailure,
}

impl ReconciliationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingProject => "missing_project",
            Self::NonAbsoluteNoncanonical => "non_absolute_noncanonical",
            Self::RegisteredRootMismatch => "registered_root_mismatch",
            Self::LexicalOrTraversalFailure => "lexical_or_traversal_failure",
            Self::EmptyRelativeIdentity => "empty_relative_identity",
            Self::LosslessRepresentationFailure => "lossless_representation_failure",
            Self::UnsupportedHostPathForm => "unsupported_host_path_form",
            Self::InternalPersistenceOrValidationFailure => {
                "internal_persistence_or_validation_failure"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSummaryReconciliationResult {
    pub run_id: i64,
    pub project_id: i64,
    pub total_examined: usize,
    pub canonical_unchanged: usize,
    pub converted: usize,
    pub conflicts: usize,
    pub unresolved_by_reason: BTreeMap<&'static str, usize>,
    pub previously_reconciled: usize,
    pub failed: usize,
    pub completed: bool,
    pub preserved_record_ids: Vec<i64>,
}

impl FileSummaryReconciliationResult {
    pub fn unresolved(&self) -> usize {
        self.unresolved_by_reason.values().sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSummaryRollbackResult {
    pub rollback_id: i64,
    pub run_id: i64,
    pub project_id: i64,
    pub restored: usize,
}

#[derive(Debug, Clone)]
struct StoredFileSummary {
    id: i64,
    project_id: i64,
    path: String,
    summary: String,
    tags_json: String,
    extracted_symbols_json: Option<String>,
    last_known_mtime: Option<i64>,
    last_known_size: Option<i64>,
    last_updated_at: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct PriorReconciliation {
    _record_id: i64,
    classification: String,
    original: StoredFileSummary,
    mapped_canonical_path: Option<String>,
    resulting_active_id: Option<i64>,
    unresolved_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IdentityClassification {
    Canonical,
    Mapped(String),
    Unresolved(ReconciliationReason),
}

pub trait FileSummaryRepository {
    fn create_or_update_file_summary(
        &self,
        summary: &NewFileSummary,
    ) -> familiar_core::Result<FileSummary>;
    fn get_file_summary_by_path(
        &self,
        project_id: i64,
        path: &str,
    ) -> familiar_core::Result<Option<FileSummary>>;
    fn list_file_summaries_by_project(
        &self,
        project_id: i64,
    ) -> familiar_core::Result<Vec<FileSummary>>;
    fn delete_file_summary(&self, id: i64) -> familiar_core::Result<()>;
}

pub(crate) fn row_to_file_summary(row: &rusqlite::Row) -> rusqlite::Result<FileSummary> {
    let tags_json: String = row.get("tags_json")?;
    let extracted_symbols_json: Option<String> = row.get("extracted_symbols_json")?;
    let last_updated_at_str: String = row.get("last_updated_at")?;
    let created_at_str: String = row.get("created_at")?;
    let updated_at_str: String = row.get("updated_at")?;

    let extracted_symbols = match extracted_symbols_json {
        Some(s) if !s.is_empty() => json_to_vec(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        _ => Vec::new(),
    };

    Ok(FileSummary {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        path: row.get("path")?,
        summary: row.get("summary")?,
        tags: json_to_vec(&tags_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        extracted_symbols,
        last_known_mtime: row.get("last_known_mtime")?,
        last_known_size: row.get("last_known_size")?,
        last_updated_at: parse_dt(&last_updated_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        created_at: parse_dt(&created_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        updated_at: parse_dt(&updated_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

impl FileSummaryRepository for Database {
    fn create_or_update_file_summary(
        &self,
        summary: &NewFileSummary,
    ) -> familiar_core::Result<FileSummary> {
        CanonicalFileIdentity::validate_stored(summary.project_id, &summary.path).map_err(|e| {
            FamiliarError::Database(format!("invalid canonical file identity: {e}"))
        })?;
        let now = now_rfc3339();
        let tags_json = vec_to_json(&summary.tags)?;
        let symbols_json = vec_to_json(&summary.extracted_symbols)?;

        self.conn()
            .execute(
                sql::UPSERT_FILE_SUMMARY,
                params![
                    summary.project_id,
                    summary.path,
                    summary.summary,
                    tags_json,
                    symbols_json,
                    summary.last_known_mtime,
                    summary.last_known_size,
                    now,
                ],
            )
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        self.get_file_summary_by_path(summary.project_id, &summary.path)?
            .ok_or_else(|| FamiliarError::Database("failed to read back file summary".into()))
    }

    fn get_file_summary_by_path(
        &self,
        project_id: i64,
        path: &str,
    ) -> familiar_core::Result<Option<FileSummary>> {
        let identity = CanonicalFileIdentity::validate_stored(project_id, path).map_err(|e| {
            FamiliarError::Database(format!("invalid canonical file identity: {e}"))
        })?;
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_FILE_SUMMARY_BY_PATH)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let canonical = stmt
            .query_row(params![project_id, identity.path()], row_to_file_summary)
            .optional()
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        // Compatibility is deliberately exact and bounded: derive the one
        // legacy absolute key from this project's registered root.
        let project =
            crate::repos::project::ProjectRepository::get_project_by_id(self, project_id)?;
        let legacy_path = project
            .map(|project| std::path::Path::new(&project.repo_root).join(identity.path()))
            .and_then(|path| path.to_str().map(str::to_owned));
        let legacy = if let Some(legacy_path) = legacy_path {
            let mut legacy_stmt = self
                .conn()
                .prepare(sql::SELECT_FILE_SUMMARY_BY_PATH)
                .map_err(|e| FamiliarError::Database(e.to_string()))?;
            legacy_stmt
                .query_row(params![project_id, legacy_path], row_to_file_summary)
                .optional()
                .map_err(|e| FamiliarError::Database(e.to_string()))?
        } else {
            None
        };

        if canonical.is_some() && legacy.is_some() {
            tracing::warn!(
                project_id,
                canonical_path = identity.path(),
                "canonical and legacy file summary records both exist"
            );
        } else if canonical.is_none() && legacy.is_some() {
            tracing::info!(
                project_id,
                canonical_path = identity.path(),
                "using exact legacy file summary fallback"
            );
        }
        Ok(canonical.or(legacy))
    }

    fn list_file_summaries_by_project(
        &self,
        project_id: i64,
    ) -> familiar_core::Result<Vec<FileSummary>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_FILE_SUMMARIES_BY_PROJECT)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![project_id], row_to_file_summary)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    fn delete_file_summary(&self, id: i64) -> familiar_core::Result<()> {
        self.conn()
            .execute(sql::DELETE_FILE_SUMMARY, params![id])
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        Ok(())
    }
}

impl Database {
    /// Reconcile legacy absolute file-summary identities for one project.
    ///
    /// This is intentionally an explicit storage-owned maintenance operation:
    /// queries never invoke it implicitly. The whole project result commits in
    /// one transaction, including complete preservation records.
    pub fn reconcile_file_summary_identities(
        &self,
        project_id: i64,
    ) -> familiar_core::Result<FileSummaryReconciliationResult> {
        self.reconcile_file_summary_identities_inner(project_id, false)
    }

    fn reconcile_file_summary_identities_inner(
        &self,
        project_id: i64,
        fail_after_preservation: bool,
    ) -> familiar_core::Result<FileSummaryReconciliationResult> {
        let run_id = self.start_file_summary_reconciliation(project_id)?;
        let tx = match self.conn().unchecked_transaction() {
            Ok(tx) => tx,
            Err(error) => {
                self.record_failed_file_summary_reconciliation(run_id)?;
                return Err(database_error(error));
            }
        };
        let result = reconcile_project(&tx, project_id, run_id, fail_after_preservation);
        match result {
            Ok(result) => {
                tx.commit().map_err(database_error)?;
                Ok(result)
            }
            Err(error) => {
                drop(tx);
                self.record_failed_file_summary_reconciliation(run_id)?;
                Err(error)
            }
        }
    }

    fn start_file_summary_reconciliation(&self, project_id: i64) -> familiar_core::Result<i64> {
        let project_exists: bool = self
            .conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
                params![project_id],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if !project_exists {
            return Err(FamiliarError::Database(format!(
                "project {project_id} does not exist"
            )));
        }
        let now = now_rfc3339();
        self.conn()
            .execute(
                "UPDATE file_summary_reconciliation_runs \
                 SET status = 'interrupted', failed = 1, completed_at = ?1 \
                 WHERE project_id = ?2 AND status = 'in_progress'",
                params![now, project_id],
            )
            .map_err(database_error)?;
        self.conn()
            .execute(
                "INSERT INTO file_summary_reconciliation_runs \
                 (project_id, status, total_examined, canonical_unchanged, converted, conflicts, \
                  unresolved, previously_reconciled, failed, started_at, completed_at) \
                 VALUES (?1, 'in_progress', 0, 0, 0, 0, 0, 0, 0, ?2, ?2)",
                params![project_id, now],
            )
            .map_err(database_error)?;
        Ok(self.conn().last_insert_rowid())
    }

    fn record_failed_file_summary_reconciliation(&self, run_id: i64) -> familiar_core::Result<()> {
        self.conn()
            .execute(
                "UPDATE file_summary_reconciliation_runs \
                 SET status = 'failed', failed = 1, completed_at = ?1 WHERE id = ?2",
                params![now_rfc3339(), run_id],
            )
            .map_err(database_error)?;
        Ok(())
    }

    /// Return the latest durable reconciliation result for a project.
    pub fn latest_file_summary_reconciliation(
        &self,
        project_id: i64,
    ) -> familiar_core::Result<Option<FileSummaryReconciliationResult>> {
        let mut statement = self
            .conn()
            .prepare(
                "SELECT id, status, total_examined, canonical_unchanged, converted, conflicts, \
                 unresolved, previously_reconciled, failed \
                 FROM file_summary_reconciliation_runs \
                 WHERE project_id = ?1 ORDER BY id DESC LIMIT 1",
            )
            .map_err(database_error)?;
        let row = statement
            .query_row(params![project_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })
            .optional()
            .map_err(database_error)?;
        let Some((run_id, status, total, canonical, converted, conflicts, _, previous, failed)) =
            row
        else {
            return Ok(None);
        };
        let (unresolved_by_reason, preserved_record_ids) =
            load_run_record_summary(self.conn(), run_id)?;
        Ok(Some(FileSummaryReconciliationResult {
            run_id,
            project_id,
            total_examined: total as usize,
            canonical_unchanged: canonical as usize,
            converted: converted as usize,
            conflicts: conflicts as usize,
            unresolved_by_reason,
            previously_reconciled: previous as usize,
            failed: failed as usize,
            completed: status == "completed",
            preserved_record_ids,
        }))
    }

    /// Restore every active outcome from one completed reconciliation run.
    /// Validation happens before mutation; a conflict records a failed rollback
    /// attempt and leaves all file-summary rows unchanged.
    pub fn rollback_file_summary_reconciliation(
        &self,
        run_id: i64,
    ) -> familiar_core::Result<FileSummaryRollbackResult> {
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(database_error)?;
        let result = rollback_run(&tx, run_id)?;
        tx.commit().map_err(database_error)?;
        match result {
            Ok(result) => Ok(result),
            Err(message) => Err(FamiliarError::Database(message)),
        }
    }
}

fn reconcile_project(
    tx: &Transaction<'_>,
    project_id: i64,
    run_id: i64,
    fail_after_preservation: bool,
) -> familiar_core::Result<FileSummaryReconciliationResult> {
    let started_at = now_rfc3339();
    let repo_root: Option<String> = tx
        .query_row(
            "SELECT repo_root FROM projects WHERE id = ?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    let repo_root = repo_root
        .ok_or_else(|| FamiliarError::Database(format!("project {project_id} does not exist")))?;

    let rows = load_project_rows(tx, project_id)?;
    let priors = load_prior_reconciliations(tx, project_id)?;
    let prior_resulting_ids: HashSet<i64> = priors
        .iter()
        .filter(|prior| prior.classification == "converted")
        .filter_map(|prior| prior.resulting_active_id)
        .collect();
    let prior_originals: HashMap<(i64, String, String), &PriorReconciliation> = priors
        .iter()
        .map(|prior| {
            (
                (
                    prior.original.id,
                    prior.original.path.clone(),
                    prior.original.updated_at.clone(),
                ),
                prior,
            )
        })
        .collect();
    let prior_archived_conflicts = priors
        .iter()
        .filter(|prior| prior.classification == "conflict")
        .count();

    let mut canonical_unchanged = 0usize;
    let mut converted = 0usize;
    let mut conflicts = 0usize;
    let mut unresolved_by_reason = BTreeMap::new();
    let mut previously_reconciled = prior_archived_conflicts;
    let mut preserved_record_ids = Vec::new();

    for row in &rows {
        let prior_original =
            prior_originals.get(&(row.id, row.path.clone(), row.updated_at.clone()));
        if prior_resulting_ids.contains(&row.id) || prior_original.is_some() {
            previously_reconciled += 1;
            if let Some(reason) =
                prior_original.and_then(|prior| prior.unresolved_reason.as_deref())
            {
                *unresolved_by_reason
                    .entry(stable_reason(reason)?)
                    .or_insert(0) += 1;
            }
            continue;
        }

        match classify_identity(project_id, Some(&repo_root), &row.path) {
            IdentityClassification::Canonical => canonical_unchanged += 1,
            IdentityClassification::Unresolved(reason) => {
                let record_id = preserve_reconciliation_record(
                    tx,
                    run_id,
                    row,
                    "unresolved",
                    Some(reason.as_str()),
                    None,
                    Some(row.id),
                    &started_at,
                )?;
                preserved_record_ids.push(record_id);
                *unresolved_by_reason.entry(reason.as_str()).or_insert(0) += 1;
            }
            IdentityClassification::Mapped(canonical_path) => {
                let canonical_id: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM file_summaries WHERE project_id = ?1 AND path = ?2",
                        params![project_id, canonical_path],
                        |query_row| query_row.get(0),
                    )
                    .optional()
                    .map_err(database_error)?;
                let classification = if canonical_id.is_some() {
                    "conflict"
                } else {
                    "converted"
                };
                let resulting_active_id = canonical_id.or(Some(row.id));
                let record_id = preserve_reconciliation_record(
                    tx,
                    run_id,
                    row,
                    classification,
                    None,
                    Some(&canonical_path),
                    resulting_active_id,
                    &started_at,
                )?;
                preserved_record_ids.push(record_id);

                if fail_after_preservation {
                    return Err(FamiliarError::Database(
                        "injected failure after reconciliation preservation".into(),
                    ));
                }

                if canonical_id.is_some() {
                    tx.execute(
                        "DELETE FROM file_summaries WHERE id = ?1 AND project_id = ?2",
                        params![row.id, project_id],
                    )
                    .map_err(database_error)?;
                    conflicts += 1;
                } else {
                    tx.execute(
                        "UPDATE file_summaries SET path = ?1 WHERE id = ?2 AND project_id = ?3",
                        params![canonical_path, row.id, project_id],
                    )
                    .map_err(database_error)?;
                    converted += 1;
                }
            }
        }
    }

    let total_examined = rows.len() + prior_archived_conflicts;
    let unresolved: usize = unresolved_by_reason.values().sum();
    let completed_at = now_rfc3339();
    tx.execute(
        "UPDATE file_summary_reconciliation_runs SET \
         status = 'completed', total_examined = ?1, canonical_unchanged = ?2, converted = ?3, conflicts = ?4, \
         unresolved = ?5, previously_reconciled = ?6, completed_at = ?7 WHERE id = ?8",
        params![
            total_examined as i64,
            canonical_unchanged as i64,
            converted as i64,
            conflicts as i64,
            unresolved as i64,
            previously_reconciled as i64,
            completed_at,
            run_id,
        ],
    )
    .map_err(database_error)?;
    for (reason, count) in &unresolved_by_reason {
        tx.execute(
            "INSERT INTO file_summary_reconciliation_run_reasons (run_id, reason, count) \
             VALUES (?1, ?2, ?3)",
            params![run_id, reason, *count as i64],
        )
        .map_err(database_error)?;
    }

    Ok(FileSummaryReconciliationResult {
        run_id,
        project_id,
        total_examined,
        canonical_unchanged,
        converted,
        conflicts,
        unresolved_by_reason,
        previously_reconciled,
        failed: 0,
        completed: true,
        preserved_record_ids,
    })
}

fn load_project_rows(
    connection: &rusqlite::Connection,
    project_id: i64,
) -> familiar_core::Result<Vec<StoredFileSummary>> {
    let mut statement = connection
        .prepare(
            "SELECT id, project_id, path, summary, tags_json, extracted_symbols_json, \
             last_known_mtime, last_known_size, last_updated_at, created_at, updated_at \
             FROM file_summaries WHERE project_id = ?1 ORDER BY path, id",
        )
        .map_err(database_error)?;
    let results = statement
        .query_map(params![project_id], stored_summary_from_row)
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    Ok(results)
}

fn stored_summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredFileSummary> {
    Ok(StoredFileSummary {
        id: row.get(0)?,
        project_id: row.get(1)?,
        path: row.get(2)?,
        summary: row.get(3)?,
        tags_json: row.get(4)?,
        extracted_symbols_json: row.get(5)?,
        last_known_mtime: row.get(6)?,
        last_known_size: row.get(7)?,
        last_updated_at: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn load_prior_reconciliations(
    connection: &rusqlite::Connection,
    project_id: i64,
) -> familiar_core::Result<Vec<PriorReconciliation>> {
    let mut statement = connection
        .prepare(
            "SELECT id, classification, unresolved_reason, mapped_canonical_path, \
             resulting_active_id, original_file_summary_id, project_id, original_path, \
             original_summary, original_tags_json, original_extracted_symbols_json, \
             original_last_known_mtime, original_last_known_size, original_last_updated_at, \
             original_created_at, original_updated_at \
             FROM file_summary_reconciliation_records \
             WHERE project_id = ?1 AND rolled_back_at IS NULL ORDER BY id",
        )
        .map_err(database_error)?;
    let results = statement
        .query_map(params![project_id], |row| {
            Ok(PriorReconciliation {
                _record_id: row.get(0)?,
                classification: row.get(1)?,
                unresolved_reason: row.get(2)?,
                mapped_canonical_path: row.get(3)?,
                resulting_active_id: row.get(4)?,
                original: StoredFileSummary {
                    id: row.get(5)?,
                    project_id: row.get(6)?,
                    path: row.get(7)?,
                    summary: row.get(8)?,
                    tags_json: row.get(9)?,
                    extracted_symbols_json: row.get(10)?,
                    last_known_mtime: row.get(11)?,
                    last_known_size: row.get(12)?,
                    last_updated_at: row.get(13)?,
                    created_at: row.get(14)?,
                    updated_at: row.get(15)?,
                },
            })
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    Ok(results)
}

#[allow(clippy::too_many_arguments)]
fn preserve_reconciliation_record(
    tx: &Transaction<'_>,
    run_id: i64,
    original: &StoredFileSummary,
    classification: &str,
    unresolved_reason: Option<&str>,
    mapped_canonical_path: Option<&str>,
    resulting_active_id: Option<i64>,
    reconciled_at: &str,
) -> familiar_core::Result<i64> {
    tx.execute(
        "INSERT INTO file_summary_reconciliation_records \
         (run_id, project_id, original_file_summary_id, classification, unresolved_reason, \
          mapped_canonical_path, resulting_active_id, original_path, original_summary, \
          original_tags_json, original_extracted_symbols_json, original_last_known_mtime, \
          original_last_known_size, original_last_updated_at, original_created_at, \
          original_updated_at, reconciled_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            run_id,
            original.project_id,
            original.id,
            classification,
            unresolved_reason,
            mapped_canonical_path,
            resulting_active_id,
            original.path,
            original.summary,
            original.tags_json,
            original.extracted_symbols_json,
            original.last_known_mtime,
            original.last_known_size,
            original.last_updated_at,
            original.created_at,
            original.updated_at,
            reconciled_at,
        ],
    )
    .map_err(database_error)?;
    Ok(tx.last_insert_rowid())
}

fn classify_identity(
    project_id: i64,
    repo_root: Option<&str>,
    stored_path: &str,
) -> IdentityClassification {
    if CanonicalFileIdentity::validate_stored(project_id, stored_path).is_ok() {
        return IdentityClassification::Canonical;
    }
    let Some(repo_root) = repo_root else {
        return IdentityClassification::Unresolved(ReconciliationReason::MissingProject);
    };
    if looks_like_unsupported_host_path(stored_path) {
        return IdentityClassification::Unresolved(ReconciliationReason::UnsupportedHostPathForm);
    }
    let stored = Path::new(stored_path);
    if !stored.is_absolute() {
        return IdentityClassification::Unresolved(ReconciliationReason::NonAbsoluteNoncanonical);
    }
    let root = Path::new(repo_root);
    if !root.is_absolute() || looks_like_unsupported_host_path(repo_root) {
        return IdentityClassification::Unresolved(ReconciliationReason::UnsupportedHostPathForm);
    }
    if has_parent_component(stored) || has_parent_component(root) {
        return IdentityClassification::Unresolved(ReconciliationReason::LexicalOrTraversalFailure);
    }
    let Ok(relative) = stored.strip_prefix(root) else {
        return IdentityClassification::Unresolved(ReconciliationReason::RegisteredRootMismatch);
    };
    if relative.as_os_str().is_empty() {
        return IdentityClassification::Unresolved(ReconciliationReason::EmptyRelativeIdentity);
    }
    let Some(relative_string) = relative.to_str() else {
        return IdentityClassification::Unresolved(
            ReconciliationReason::LosslessRepresentationFailure,
        );
    };
    let Ok(identity) = CanonicalFileIdentity::validate_stored(project_id, relative_string) else {
        return IdentityClassification::Unresolved(ReconciliationReason::LexicalOrTraversalFailure);
    };
    let reconstructed = root.join(identity.path());
    let Some(reconstructed) = reconstructed.to_str() else {
        return IdentityClassification::Unresolved(
            ReconciliationReason::LosslessRepresentationFailure,
        );
    };
    if reconstructed != stored_path {
        return IdentityClassification::Unresolved(ReconciliationReason::LexicalOrTraversalFailure);
    }
    IdentityClassification::Mapped(identity.path().to_owned())
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn looks_like_unsupported_host_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/'))
        || path.starts_with("\\\\")
}

fn load_run_record_summary(
    connection: &rusqlite::Connection,
    run_id: i64,
) -> familiar_core::Result<(BTreeMap<&'static str, usize>, Vec<i64>)> {
    let mut reasons = BTreeMap::new();
    let mut reason_statement = connection
        .prepare(
            "SELECT reason, count FROM file_summary_reconciliation_run_reasons \
             WHERE run_id = ?1 ORDER BY reason",
        )
        .map_err(database_error)?;
    let reason_rows = reason_statement
        .query_map(params![run_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(database_error)?;
    for reason in reason_rows {
        let (reason, count) = reason.map_err(database_error)?;
        reasons.insert(stable_reason(&reason)?, count as usize);
    }

    let mut record_statement = connection
        .prepare(
            "SELECT id FROM file_summary_reconciliation_records \
             WHERE run_id = ?1 ORDER BY id",
        )
        .map_err(database_error)?;
    let record_ids = record_statement
        .query_map(params![run_id], |row| row.get(0))
        .map_err(database_error)?
        .collect::<Result<Vec<i64>, _>>()
        .map_err(database_error)?;
    Ok((reasons, record_ids))
}

fn stable_reason(reason: &str) -> familiar_core::Result<&'static str> {
    match reason {
        "missing_project" => Ok("missing_project"),
        "non_absolute_noncanonical" => Ok("non_absolute_noncanonical"),
        "registered_root_mismatch" => Ok("registered_root_mismatch"),
        "lexical_or_traversal_failure" => Ok("lexical_or_traversal_failure"),
        "empty_relative_identity" => Ok("empty_relative_identity"),
        "lossless_representation_failure" => Ok("lossless_representation_failure"),
        "unsupported_host_path_form" => Ok("unsupported_host_path_form"),
        "internal_persistence_or_validation_failure" => {
            Ok("internal_persistence_or_validation_failure")
        }
        other => Err(FamiliarError::Database(format!(
            "unknown reconciliation reason {other:?}"
        ))),
    }
}

fn rollback_run(
    tx: &Transaction<'_>,
    run_id: i64,
) -> familiar_core::Result<Result<FileSummaryRollbackResult, String>> {
    let project_id: Option<i64> = tx
        .query_row(
            "SELECT project_id FROM file_summary_reconciliation_runs \
             WHERE id = ?1 AND status = 'completed'",
            params![run_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    let Some(project_id) = project_id else {
        return Err(FamiliarError::Database(format!(
            "completed reconciliation run {run_id} does not exist"
        )));
    };
    let records = load_run_reconciliations(tx, run_id)?;
    if records.is_empty() {
        return record_rollback_conflict(tx, run_id, project_id, "run has no restorable records");
    }
    if records
        .iter()
        .all(|record| record.classification == "unresolved")
    {
        return record_rollback_conflict(
            tx,
            run_id,
            project_id,
            "run contains only unresolved records and changed no active identity",
        );
    }

    for record in &records {
        if record.classification == "unresolved" {
            continue;
        }
        if record.classification == "converted" {
            let Some(mapped_path) = record.mapped_canonical_path.as_deref() else {
                return record_rollback_conflict(tx, run_id, project_id, "missing mapped path");
            };
            let current = load_summary_by_id(tx, record.original.id)?;
            let Some(current) = current else {
                return record_rollback_conflict(
                    tx,
                    run_id,
                    project_id,
                    "converted active row is missing",
                );
            };
            if current.path != mapped_path || !same_non_path_fields(&current, &record.original) {
                return record_rollback_conflict(
                    tx,
                    run_id,
                    project_id,
                    "converted active row changed after reconciliation",
                );
            }
        } else if load_summary_by_id(tx, record.original.id)?.is_some() {
            return record_rollback_conflict(
                tx,
                run_id,
                project_id,
                "original conflict row id is occupied",
            );
        }
        let path_owner: Option<i64> = tx
            .query_row(
                "SELECT id FROM file_summaries WHERE project_id = ?1 AND path = ?2",
                params![project_id, record.original.path],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        if path_owner.is_some_and(|owner| owner != record.original.id) {
            return record_rollback_conflict(
                tx,
                run_id,
                project_id,
                "original legacy path is occupied",
            );
        }
    }

    let rollback_at = now_rfc3339();
    let mut restored = 0usize;
    for record in &records {
        match record.classification.as_str() {
            "converted" => {
                tx.execute(
                    "UPDATE file_summaries SET path = ?1 WHERE id = ?2 AND project_id = ?3",
                    params![record.original.path, record.original.id, project_id],
                )
                .map_err(database_error)?;
                restored += 1;
            }
            "conflict" => {
                insert_exact_summary(tx, &record.original)?;
                restored += 1;
            }
            "unresolved" => {}
            other => {
                return Err(FamiliarError::Database(format!(
                    "unsupported reconciliation classification {other:?}"
                )))
            }
        }
    }
    tx.execute(
        "UPDATE file_summary_reconciliation_records SET rolled_back_at = ?1 \
         WHERE run_id = ?2 AND rolled_back_at IS NULL",
        params![rollback_at, run_id],
    )
    .map_err(database_error)?;
    tx.execute(
        "INSERT INTO file_summary_reconciliation_rollbacks \
         (run_id, project_id, outcome, conflict_reason, recorded_at) \
         VALUES (?1, ?2, 'succeeded', NULL, ?3)",
        params![run_id, project_id, rollback_at],
    )
    .map_err(database_error)?;
    let rollback_id = tx.last_insert_rowid();
    Ok(Ok(FileSummaryRollbackResult {
        rollback_id,
        run_id,
        project_id,
        restored,
    }))
}

fn load_run_reconciliations(
    connection: &rusqlite::Connection,
    run_id: i64,
) -> familiar_core::Result<Vec<PriorReconciliation>> {
    let mut statement = connection
        .prepare(
            "SELECT id, classification, unresolved_reason, mapped_canonical_path, \
             resulting_active_id, original_file_summary_id, project_id, original_path, \
             original_summary, original_tags_json, original_extracted_symbols_json, \
             original_last_known_mtime, original_last_known_size, original_last_updated_at, \
             original_created_at, original_updated_at \
             FROM file_summary_reconciliation_records \
             WHERE run_id = ?1 AND rolled_back_at IS NULL ORDER BY id",
        )
        .map_err(database_error)?;
    let results = statement
        .query_map(params![run_id], |row| {
            Ok(PriorReconciliation {
                _record_id: row.get(0)?,
                classification: row.get(1)?,
                unresolved_reason: row.get(2)?,
                mapped_canonical_path: row.get(3)?,
                resulting_active_id: row.get(4)?,
                original: StoredFileSummary {
                    id: row.get(5)?,
                    project_id: row.get(6)?,
                    path: row.get(7)?,
                    summary: row.get(8)?,
                    tags_json: row.get(9)?,
                    extracted_symbols_json: row.get(10)?,
                    last_known_mtime: row.get(11)?,
                    last_known_size: row.get(12)?,
                    last_updated_at: row.get(13)?,
                    created_at: row.get(14)?,
                    updated_at: row.get(15)?,
                },
            })
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    Ok(results)
}

fn load_summary_by_id(
    connection: &rusqlite::Connection,
    id: i64,
) -> familiar_core::Result<Option<StoredFileSummary>> {
    connection
        .query_row(
            "SELECT id, project_id, path, summary, tags_json, extracted_symbols_json, \
             last_known_mtime, last_known_size, last_updated_at, created_at, updated_at \
             FROM file_summaries WHERE id = ?1",
            params![id],
            stored_summary_from_row,
        )
        .optional()
        .map_err(database_error)
}

fn same_non_path_fields(left: &StoredFileSummary, right: &StoredFileSummary) -> bool {
    left.id == right.id
        && left.project_id == right.project_id
        && left.summary == right.summary
        && left.tags_json == right.tags_json
        && left.extracted_symbols_json == right.extracted_symbols_json
        && left.last_known_mtime == right.last_known_mtime
        && left.last_known_size == right.last_known_size
        && left.last_updated_at == right.last_updated_at
        && left.created_at == right.created_at
        && left.updated_at == right.updated_at
}

fn insert_exact_summary(
    tx: &Transaction<'_>,
    summary: &StoredFileSummary,
) -> familiar_core::Result<()> {
    tx.execute(
        "INSERT INTO file_summaries \
         (id, project_id, path, summary, tags_json, extracted_symbols_json, last_known_mtime, \
          last_known_size, last_updated_at, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            summary.id,
            summary.project_id,
            summary.path,
            summary.summary,
            summary.tags_json,
            summary.extracted_symbols_json,
            summary.last_known_mtime,
            summary.last_known_size,
            summary.last_updated_at,
            summary.created_at,
            summary.updated_at,
        ],
    )
    .map_err(database_error)?;
    Ok(())
}

fn record_rollback_conflict(
    tx: &Transaction<'_>,
    run_id: i64,
    project_id: i64,
    message: &str,
) -> familiar_core::Result<Result<FileSummaryRollbackResult, String>> {
    tx.execute(
        "INSERT INTO file_summary_reconciliation_rollbacks \
         (run_id, project_id, outcome, conflict_reason, recorded_at) \
         VALUES (?1, ?2, 'conflict', ?3, ?4)",
        params![run_id, project_id, message, now_rfc3339()],
    )
    .map_err(database_error)?;
    Ok(Err(message.to_owned()))
}

fn database_error(error: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}

/// Query file summaries under a path prefix, with a SQL-side limit.
/// Used by the MCP `get_module_summary` tool.
pub fn list_file_summaries_under(
    db: &Database,
    project_id: i64,
    path_prefix: &str,
    limit: usize,
) -> familiar_core::Result<Vec<FileSummary>> {
    validate_module_prefix(path_prefix)?;
    let mut stmt = db
        .conn()
        .prepare(sql::SELECT_FILE_SUMMARIES_UNDER)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(
            params![project_id, path_prefix, limit as i64],
            row_to_file_summary,
        )
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
    }
    Ok(results)
}

/// Count file summaries under a path prefix without retrieving rows.
/// Used by the MCP `get_module_summary` tool to report true file_count.
pub fn count_file_summaries_under(
    db: &Database,
    project_id: i64,
    path_prefix: &str,
) -> familiar_core::Result<usize> {
    validate_module_prefix(path_prefix)?;
    let mut stmt = db
        .conn()
        .prepare(sql::COUNT_FILE_SUMMARIES_UNDER)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let count: i64 = stmt
        .query_row(params![project_id, path_prefix], |row| row.get(0))
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    Ok(count as usize)
}

fn validate_module_prefix(path_prefix: &str) -> familiar_core::Result<()> {
    let input = path_prefix.strip_suffix('/').unwrap_or(path_prefix);
    let canonical = CanonicalFileIdentity::module_prefix(std::path::Path::new(input))
        .map_err(|e| FamiliarError::Database(format!("invalid canonical module identity: {e}")))?;
    if canonical != path_prefix {
        return Err(FamiliarError::Database(
            "module prefix is not canonical".into(),
        ));
    }
    Ok(())
}

/// Search file summaries by keyword (case-insensitive LIKE across summary, symbols, path).
pub fn search_file_summaries(
    db: &Database,
    project_id: i64,
    query: &str,
    limit: usize,
) -> familiar_core::Result<Vec<FileSummary>> {
    let mut stmt = db
        .conn()
        .prepare(sql::SEARCH_FILE_SUMMARIES)
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(
            params![project_id, query, limit as i64],
            row_to_file_summary,
        )
        .map_err(|e| FamiliarError::Database(e.to_string()))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::project::ProjectRepository;
    use familiar_core::models::NewProject;

    fn test_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    fn create_test_project(db: &Database) -> i64 {
        db.create_project(&NewProject {
            name: "test".into(),
            repo_root: "/test/project".into(),
            ignored_paths: vec![],
            token_budget: None,
        })
        .unwrap()
        .id
    }

    fn sample_summary(project_id: i64) -> NewFileSummary {
        NewFileSummary {
            project_id,
            path: "src/main.rs".into(),
            summary: "Main entry point".into(),
            tags: vec!["entry".into(), "code".into()],
            extracted_symbols: vec!["main".into(), "Config".into()],
            last_known_mtime: Some(1_700_000_000),
            last_known_size: Some(1024),
        }
    }

    fn insert_legacy(db: &Database, project_id: i64, path: &str, summary: &str) -> i64 {
        let now = "2026-01-02T03:04:05Z";
        db.conn()
            .execute(
                "INSERT INTO file_summaries \
                 (project_id, path, summary, tags_json, extracted_symbols_json, last_known_mtime, \
                  last_known_size, last_updated_at, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, '[\"legacy-tag\"]', '[\"legacy_symbol\"]', \
                         123, 456, ?4, ?4, ?4)",
                params![project_id, path, summary, now],
            )
            .unwrap();
        db.conn().last_insert_rowid()
    }

    #[test]
    fn create_and_get_by_path() {
        let db = test_db();
        let pid = create_test_project(&db);
        let created = db
            .create_or_update_file_summary(&sample_summary(pid))
            .unwrap();
        assert_eq!(created.path, "src/main.rs");
        assert_eq!(created.summary, "Main entry point");
        assert_eq!(created.tags, vec!["entry", "code"]);
        assert_eq!(created.extracted_symbols, vec!["main", "Config"]);
        assert_eq!(created.last_known_mtime, Some(1_700_000_000));
        assert_eq!(created.last_known_size, Some(1024));

        let fetched = db
            .get_file_summary_by_path(pid, "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.extracted_symbols, vec!["main", "Config"]);
    }

    #[test]
    fn persistence_rejects_noncanonical_paths() {
        let db = test_db();
        let pid = create_test_project(&db);
        for path in [
            "",
            "/test/project/src/main.rs",
            "../main.rs",
            "src/./main.rs",
            "src//main.rs",
        ] {
            let mut summary = sample_summary(pid);
            summary.path = path.into();
            assert!(
                db.create_or_update_file_summary(&summary).is_err(),
                "accepted {path:?}"
            );
        }
        assert!(db.list_file_summaries_by_project(pid).unwrap().is_empty());
    }

    #[test]
    fn canonical_lookup_precedes_exact_legacy_fallback_without_mutation() {
        let db = test_db();
        let pid = create_test_project(&db);
        let mut legacy = sample_summary(pid);
        legacy.path = "/test/project/src/main.rs".into();
        let now = now_rfc3339();
        db.conn()
            .execute(
                sql::UPSERT_FILE_SUMMARY,
                params![
                    legacy.project_id,
                    legacy.path,
                    "legacy",
                    "[]",
                    "[]",
                    Option::<i64>::None,
                    Option::<i64>::None,
                    now,
                ],
            )
            .unwrap();

        let fallback = db
            .get_file_summary_by_path(pid, "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(fallback.path, "/test/project/src/main.rs");
        assert_eq!(db.list_file_summaries_by_project(pid).unwrap().len(), 1);

        db.create_or_update_file_summary(&sample_summary(pid))
            .unwrap();
        let canonical = db
            .get_file_summary_by_path(pid, "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(canonical.path, "src/main.rs");
        let rows = db.list_file_summaries_by_project(pid).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .any(|row| row.path == "/test/project/src/main.rs"));
    }

    #[test]
    fn reconciliation_converts_preserves_conflicts_and_reports_unresolved() {
        let db = test_db();
        let pid = create_test_project(&db);
        let converted_id =
            insert_legacy(&db, pid, "/test/project/src/convert.rs", "convert legacy");
        let conflict_id = insert_legacy(
            &db,
            pid,
            "/test/project/src/main.rs",
            "conflicting legacy payload",
        );
        let unresolved_id = insert_legacy(&db, pid, "/test/project-other/a.rs", "unresolved");
        let canonical = db
            .create_or_update_file_summary(&sample_summary(pid))
            .unwrap();

        let result = db.reconcile_file_summary_identities(pid).unwrap();
        assert!(result.completed);
        assert_eq!(result.total_examined, 4);
        assert_eq!(result.canonical_unchanged, 1);
        assert_eq!(result.converted, 1);
        assert_eq!(result.conflicts, 1);
        assert_eq!(result.unresolved(), 1);
        assert_eq!(result.previously_reconciled, 0);
        assert_eq!(result.failed, 0);
        assert_eq!(result.unresolved_by_reason["registered_root_mismatch"], 1);
        assert_eq!(result.preserved_record_ids.len(), 3);

        let converted = load_summary_by_id(db.conn(), converted_id)
            .unwrap()
            .unwrap();
        assert_eq!(converted.path, "src/convert.rs");
        assert_eq!(converted.summary, "convert legacy");
        assert_eq!(converted.tags_json, "[\"legacy-tag\"]");
        assert_eq!(
            converted.extracted_symbols_json.as_deref(),
            Some("[\"legacy_symbol\"]")
        );
        assert_eq!(converted.last_known_mtime, Some(123));
        assert_eq!(converted.last_known_size, Some(456));
        assert!(load_summary_by_id(db.conn(), conflict_id)
            .unwrap()
            .is_none());
        assert_eq!(
            load_summary_by_id(db.conn(), unresolved_id)
                .unwrap()
                .unwrap()
                .path,
            "/test/project-other/a.rs"
        );
        assert_eq!(
            db.get_file_summary_by_path(pid, "src/main.rs")
                .unwrap()
                .unwrap()
                .id,
            canonical.id
        );
        assert_eq!(count_file_summaries_under(&db, pid, "src/").unwrap(), 2);
        assert_eq!(
            crate::repos::stats::global_stats(&db)
                .unwrap()
                .file_summaries,
            3
        );
        assert_eq!(
            search_file_summaries(&db, pid, "conflicting", 10)
                .unwrap()
                .len(),
            0
        );

        let archived: (String, String, String, String, Option<i64>, Option<i64>) = db
            .conn()
            .query_row(
                "SELECT original_path, original_summary, original_tags_json, \
                 original_extracted_symbols_json, original_last_known_mtime, original_last_known_size \
                 FROM file_summary_reconciliation_records \
                 WHERE original_file_summary_id = ?1",
                params![conflict_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(archived.0, "/test/project/src/main.rs");
        assert_eq!(archived.1, "conflicting legacy payload");
        assert_eq!(archived.2, "[\"legacy-tag\"]");
        assert_eq!(archived.3, "[\"legacy_symbol\"]");
        assert_eq!(archived.4, Some(123));
        assert_eq!(archived.5, Some(456));

        assert_eq!(
            db.conn()
                .query_row("PRAGMA foreign_key_check", [], |_| Ok(1))
                .optional()
                .unwrap(),
            None
        );
        let integrity: String = db
            .conn()
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");

        let latest = db.latest_file_summary_reconciliation(pid).unwrap().unwrap();
        assert_eq!(latest, result);
    }

    #[test]
    fn reconciliation_is_idempotent_and_processes_late_candidates() {
        let db = test_db();
        let pid = create_test_project(&db);
        insert_legacy(&db, pid, "/test/project/a.rs", "first");
        let first = db.reconcile_file_summary_identities(pid).unwrap();
        assert_eq!(first.converted, 1);

        let second = db.reconcile_file_summary_identities(pid).unwrap();
        assert_eq!(second.converted, 0);
        assert_eq!(second.conflicts, 0);
        assert_eq!(second.previously_reconciled, 1);
        let preserved_count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM file_summary_reconciliation_records WHERE project_id = ?1",
                params![pid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved_count, 1);

        insert_legacy(&db, pid, "/test/project/b.rs", "late");
        let third = db.reconcile_file_summary_identities(pid).unwrap();
        assert_eq!(third.converted, 1);
        assert_eq!(third.previously_reconciled, 1);
        assert_eq!(db.list_file_summaries_by_project(pid).unwrap().len(), 2);
    }

    #[test]
    fn injected_failure_rolls_back_preservation_and_active_mutation() {
        let db = test_db();
        let pid = create_test_project(&db);
        let legacy_id = insert_legacy(&db, pid, "/test/project/a.rs", "legacy");
        let error = db
            .reconcile_file_summary_identities_inner(pid, true)
            .unwrap_err();
        assert!(error.to_string().contains("injected failure"));
        assert_eq!(
            load_summary_by_id(db.conn(), legacy_id)
                .unwrap()
                .unwrap()
                .path,
            "/test/project/a.rs"
        );
        let preserved_count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM file_summary_reconciliation_records",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved_count, 0);
        let latest = db.latest_file_summary_reconciliation(pid).unwrap().unwrap();
        assert!(!latest.completed);
        assert_eq!(latest.failed, 1);
    }

    #[test]
    fn retry_marks_abandoned_run_interrupted_before_new_work() {
        let db = test_db();
        let pid = create_test_project(&db);
        let abandoned = db.start_file_summary_reconciliation(pid).unwrap();
        insert_legacy(&db, pid, "/test/project/a.rs", "legacy");

        let completed = db.reconcile_file_summary_identities(pid).unwrap();
        assert!(completed.completed);
        let abandoned_status: (String, i64) = db
            .conn()
            .query_row(
                "SELECT status, failed FROM file_summary_reconciliation_runs WHERE id = ?1",
                params![abandoned],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(abandoned_status, ("interrupted".into(), 1));
    }

    #[test]
    fn rollback_restores_exact_rows_and_refuses_later_state_conflict() {
        let db = test_db();
        let pid = create_test_project(&db);
        let converted_id = insert_legacy(&db, pid, "/test/project/a.rs", "converted");
        let conflict_id = insert_legacy(&db, pid, "/test/project/src/main.rs", "conflict");
        db.create_or_update_file_summary(&sample_summary(pid))
            .unwrap();
        let run = db.reconcile_file_summary_identities(pid).unwrap();

        let rollback = db.rollback_file_summary_reconciliation(run.run_id).unwrap();
        assert_eq!(rollback.restored, 2);
        assert_eq!(
            load_summary_by_id(db.conn(), converted_id)
                .unwrap()
                .unwrap()
                .path,
            "/test/project/a.rs"
        );
        assert_eq!(
            load_summary_by_id(db.conn(), conflict_id)
                .unwrap()
                .unwrap()
                .summary,
            "conflict"
        );

        let rerun = db.reconcile_file_summary_identities(pid).unwrap();
        db.conn()
            .execute(
                "UPDATE file_summaries SET summary = 'later state' WHERE id = ?1",
                params![converted_id],
            )
            .unwrap();
        let error = db
            .rollback_file_summary_reconciliation(rerun.run_id)
            .unwrap_err();
        assert!(error.to_string().contains("changed after reconciliation"));
        assert_eq!(
            load_summary_by_id(db.conn(), converted_id)
                .unwrap()
                .unwrap()
                .summary,
            "later state"
        );
        let conflict_attempts: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM file_summary_reconciliation_rollbacks \
                 WHERE run_id = ?1 AND outcome = 'conflict'",
                params![rerun.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(conflict_attempts, 1);
    }

    #[test]
    fn classification_is_lexical_project_scoped_and_does_not_require_files() {
        assert_eq!(
            classify_identity(1, None, "/project/a.rs"),
            IdentityClassification::Unresolved(ReconciliationReason::MissingProject)
        );
        assert_eq!(
            classify_identity(1, Some("/project"), "src/./a.rs"),
            IdentityClassification::Unresolved(ReconciliationReason::NonAbsoluteNoncanonical)
        );
        assert_eq!(
            classify_identity(1, Some("C:\\project"), "/project/a.rs"),
            IdentityClassification::Unresolved(ReconciliationReason::UnsupportedHostPathForm)
        );
        assert_eq!(
            classify_identity(1, Some("/project"), "/project"),
            IdentityClassification::Unresolved(ReconciliationReason::EmptyRelativeIdentity)
        );
        assert_eq!(
            classify_identity(1, Some("/project"), "/project/missing/a.rs"),
            IdentityClassification::Mapped("missing/a.rs".into())
        );
        assert_eq!(
            classify_identity(1, Some("/project"), "/project-other/a.rs"),
            IdentityClassification::Unresolved(ReconciliationReason::RegisteredRootMismatch)
        );
        assert_eq!(
            classify_identity(1, Some("/project"), "/project/../outside.rs"),
            IdentityClassification::Unresolved(ReconciliationReason::LexicalOrTraversalFailure)
        );
    }

    #[test]
    fn reconciliation_is_project_isolated_and_canonical_only_is_unchanged() {
        let db = test_db();
        let first = create_test_project(&db);
        let second = db
            .create_project(&NewProject {
                name: "second".into(),
                repo_root: "/second/project".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;
        let first_id = insert_legacy(&db, first, "/test/project/src/main.rs", "first");
        let second_id = insert_legacy(&db, second, "/second/project/src/main.rs", "second");
        let first_result = db.reconcile_file_summary_identities(first).unwrap();
        assert_eq!(first_result.converted, 1);
        assert_eq!(
            load_summary_by_id(db.conn(), first_id)
                .unwrap()
                .unwrap()
                .path,
            "src/main.rs"
        );
        assert_eq!(
            load_summary_by_id(db.conn(), second_id)
                .unwrap()
                .unwrap()
                .path,
            "/second/project/src/main.rs"
        );
        let second_result = db.reconcile_file_summary_identities(second).unwrap();
        assert_eq!(second_result.converted, 1);

        let canonical_only = db.reconcile_file_summary_identities(first).unwrap();
        assert_eq!(canonical_only.converted, 0);
        assert_eq!(canonical_only.conflicts, 0);
        assert_eq!(canonical_only.unresolved(), 0);
    }

    #[test]
    fn upsert_updates_existing() {
        let db = test_db();
        let pid = create_test_project(&db);
        let created = db
            .create_or_update_file_summary(&sample_summary(pid))
            .unwrap();

        let updated_input = NewFileSummary {
            project_id: pid,
            path: "src/main.rs".into(),
            summary: "Updated summary".into(),
            tags: vec!["updated".into()],
            extracted_symbols: vec!["new_fn".into()],
            last_known_mtime: Some(1_700_000_500),
            last_known_size: Some(2048),
        };
        let updated = db.create_or_update_file_summary(&updated_input).unwrap();
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.summary, "Updated summary");
        assert_eq!(updated.tags, vec!["updated"]);
        assert_eq!(updated.extracted_symbols, vec!["new_fn"]);
        assert_eq!(updated.last_known_mtime, Some(1_700_000_500));
        assert_eq!(updated.last_known_size, Some(2048));
    }

    #[test]
    fn list_by_project() {
        let db = test_db();
        let pid = create_test_project(&db);
        db.create_or_update_file_summary(&sample_summary(pid))
            .unwrap();
        db.create_or_update_file_summary(&NewFileSummary {
            project_id: pid,
            path: "src/lib.rs".into(),
            summary: "Library root".into(),
            tags: vec![],
            extracted_symbols: vec![],
            last_known_mtime: None,
            last_known_size: None,
        })
        .unwrap();

        let all = db.list_file_summaries_by_project(pid).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn list_under_prefix_with_limit() {
        let db = test_db();
        let pid = create_test_project(&db);
        for path in ["src/a.rs", "src/b.rs", "src/sub/c.rs", "tests/d.rs"] {
            db.create_or_update_file_summary(&NewFileSummary {
                project_id: pid,
                path: path.into(),
                summary: "x".into(),
                tags: vec![],
                extracted_symbols: vec![],
                last_known_mtime: None,
                last_known_size: None,
            })
            .unwrap();
        }

        let under_src = list_file_summaries_under(&db, pid, "src/", 100).unwrap();
        assert_eq!(under_src.len(), 3);

        let under_src_limited = list_file_summaries_under(&db, pid, "src/", 2).unwrap();
        assert_eq!(under_src_limited.len(), 2);

        let under_tests = list_file_summaries_under(&db, pid, "tests/", 100).unwrap();
        assert_eq!(under_tests.len(), 1);
    }

    #[test]
    fn count_under_prefix() {
        let db = test_db();
        let pid = create_test_project(&db);
        for path in ["src/a.rs", "src/b.rs", "src/sub/c.rs", "tests/d.rs"] {
            db.create_or_update_file_summary(&NewFileSummary {
                project_id: pid,
                path: path.into(),
                summary: "x".into(),
                tags: vec![],
                extracted_symbols: vec![],
                last_known_mtime: None,
                last_known_size: None,
            })
            .unwrap();
        }

        assert_eq!(count_file_summaries_under(&db, pid, "src/").unwrap(), 3);
        assert_eq!(count_file_summaries_under(&db, pid, "tests/").unwrap(), 1);
        assert_eq!(count_file_summaries_under(&db, pid, "nope/").unwrap(), 0);
    }

    #[test]
    fn project_isolation() {
        let db = test_db();
        let pid1 = create_test_project(&db);

        let pid2 = db
            .create_project(&NewProject {
                name: "other".into(),
                repo_root: "/other/project".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;

        db.create_or_update_file_summary(&sample_summary(pid1))
            .unwrap();
        db.create_or_update_file_summary(&NewFileSummary {
            project_id: pid2,
            path: "src/main.rs".into(),
            summary: "Other project main".into(),
            tags: vec![],
            extracted_symbols: vec![],
            last_known_mtime: None,
            last_known_size: None,
        })
        .unwrap();

        let p1_summaries = db.list_file_summaries_by_project(pid1).unwrap();
        let p2_summaries = db.list_file_summaries_by_project(pid2).unwrap();
        assert_eq!(p1_summaries.len(), 1);
        assert_eq!(p2_summaries.len(), 1);
        assert_eq!(p1_summaries[0].summary, "Main entry point");
        assert_eq!(p2_summaries[0].summary, "Other project main");
    }

    #[test]
    fn delete_file_summary() {
        let db = test_db();
        let pid = create_test_project(&db);
        let created = db
            .create_or_update_file_summary(&sample_summary(pid))
            .unwrap();
        db.delete_file_summary(created.id).unwrap();
        assert!(db
            .get_file_summary_by_path(pid, "src/main.rs")
            .unwrap()
            .is_none());
    }
}
