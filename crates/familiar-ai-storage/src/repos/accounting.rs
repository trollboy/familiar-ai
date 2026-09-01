use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{params, Connection, OptionalExtension};

use familiar_ai_core::{FamiliarError, ResourceType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageBucket {
    Hour,
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageSeriesRequest {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub bucket: UsageBucket,
    pub group_by: Vec<String>,
    pub filters: BTreeMap<String, String>,
    pub dense: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageSeriesPoint {
    pub bucket_start: String,
    pub bucket_end: String,
    pub dimensions: BTreeMap<String, String>,
    pub estimated_cost_nanousd: Option<i64>,
    pub authoritative_cost_nanousd: Option<i64>,
    pub input_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub request_count: Option<i64>,
    pub credit_estimate_microcredits: Option<i64>,
    pub observation_ids: Vec<String>,
}

type SeriesGroup = Vec<(String, String)>;
type SeriesKey = (DateTime<Utc>, SeriesGroup);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectIdentityResolution {
    Resolved {
        project_id: String,
    },
    Degraded {
        evidence: Option<String>,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct OpenAiCostFact<'a> {
    pub source_id: &'a str,
    pub organization_id: &'a str,
    pub project_id: Option<&'a str>,
    pub bucket_start: i64,
    pub bucket_end: i64,
    pub line_item: &'a str,
    pub classification: &'a str,
    pub raw_amount_lexical: &'a str,
    pub currency: &'a str,
    pub payload_hash: &'a str,
    pub collected_at: &'a str,
}

#[derive(Debug, Clone)]
pub struct UsageObservation<'a> {
    pub execution_id: &'a str,
    pub attempt_id: &'a str,
    pub stage: &'a str,
    pub session_id: Option<&'a str>,
    pub worker_identity: &'a str,
    pub adapter: &'a str,
    pub cli_version: Option<&'a str>,
    pub model_identity: Option<&'a str>,
    pub service_tier: Option<&'a str>,
    pub provider_request_id: Option<&'a str>,
    pub uncached_input_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub unknown_reason: Option<&'a str>,
    pub period_start: &'a str,
    pub period_end: &'a str,
    pub terminal_status: &'a str,
    pub source_event_hash: &'a str,
    pub provider_cost_lexical: Option<&'a str>,
    /// Canonical Git common-directory evidence; absent is explicitly degraded.
    pub project_resolution_evidence: Option<&'a str>,
    pub output_register_id: &'a str,
    pub output_register_version: &'a str,
    pub input_compression_id: &'a str,
    pub input_compression_version: &'a str,
    pub compression_experiment: Option<&'a str>,
    pub compression_lane: Option<&'a str>,
}

/// A closed reconciliation status vocabulary (PRD-053). Reconciliation rows
/// are new append-only facts derived from PRD-051 local estimates and the
/// PRD-052 current-effective provider projection; they never edit either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationStatus {
    Reconciled,
    ReconciledWithVariance,
    Pending,
    Mismatch,
    UnattributedProviderSpend,
}

impl ReconciliationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reconciled => "reconciled",
            Self::ReconciledWithVariance => "reconciled-with-variance",
            Self::Pending => "pending",
            Self::Mismatch => "mismatch",
            Self::UnattributedProviderSpend => "unattributed-provider-spend",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "reconciled" => Self::Reconciled,
            "reconciled-with-variance" => Self::ReconciledWithVariance,
            "pending" => Self::Pending,
            "mismatch" => Self::Mismatch,
            "unattributed-provider-spend" => Self::UnattributedProviderSpend,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReconciliationRow {
    pub row_id: String,
    pub run_id: String,
    pub billing_source: String,
    pub day_start: String,
    pub day_end: String,
    pub match_key: String,
    pub project_id: Option<String>,
    pub status: String,
    pub local_estimate_nanousd: Option<i64>,
    pub authoritative_nanousd: Option<i64>,
    pub variance_nanousd: Option<i64>,
    pub tolerance_nanousd: i64,
    pub provider_revision_ids: Vec<String>,
    pub observation_ids: Vec<String>,
    pub reservation_evidence_count: i64,
    pub reservation_evidence_nanousd: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReconciliationSummary {
    pub run_id: String,
    pub rows_appended: usize,
    pub rows_unchanged: usize,
    pub rows: Vec<ReconciliationRow>,
}

/// Month-to-date cost for one billing source. Every monetary field is kept
/// distinct by authority so a caller can never sum an estimate and an
/// authoritative figure into one ambiguous number; `completeness` and
/// `freshness` label how current and settled the figures are.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceMonthSummary {
    pub billing_source: String,
    pub coverage_start: String,
    pub coverage_end: String,
    pub authoritative_nanousd: Option<i64>,
    pub local_estimate_nanousd: Option<i64>,
    pub unattributed_nanousd: Option<i64>,
    pub reconciled_days: i64,
    pub reconciled_with_variance_days: i64,
    pub pending_days: i64,
    pub mismatch_days: i64,
    pub unattributed_provider_spend_days: i64,
    pub completeness: String,
    pub freshness: String,
}

/// A month-to-date aggregate across billing sources. Present alongside the
/// per-source breakdown (`MonthToDateReport::sources`), never instead of it,
/// so a caller can never lose which source contributed what.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct AggregateMonthSummary {
    pub coverage_start: String,
    pub coverage_end: String,
    pub authoritative_nanousd: Option<i64>,
    pub local_estimate_nanousd: Option<i64>,
    pub unattributed_nanousd: Option<i64>,
    pub source_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct MonthToDateReport {
    pub sources: Vec<SourceMonthSummary>,
    pub aggregate: AggregateMonthSummary,
}

/// PRD-032 scoring input: cost per (PRD, worker). Reconciliation's grain is
/// workspace-day, not per-execution, so authority here is `"estimated"` (from
/// local cost_estimates) or `"unknown"` (no cost_estimates row exists at
/// all) — never `"authoritative"`. Callers ranking workers by cost MUST
/// treat `authority == "unknown"` as unrankable, never as free/zero.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PrdCostScoreInput {
    pub prd: String,
    pub worker_identity: String,
    pub local_estimate_nanousd: Option<i64>,
    pub authority: String,
    pub completeness: String,
}

pub struct AccountingRepository<'a> {
    conn: &'a Connection,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LedgerUsageSummary {
    pub observations: u64,
    pub unknown_observations: u64,
    pub uncached_input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub known_nanousd: u64,
    pub vendor_reported_estimates: u64,
    pub configured_rate_estimates: u64,
    pub known_zero_estimates: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompressionLaneSummary {
    pub observations: u64,
    pub uncached_input_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub known_nanousd: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressionExperimentSummary {
    pub off: CompressionLaneSummary,
    pub on: CompressionLaneSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSink {
    pub dimension: String,
    pub value: String,
    pub category: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEffect {
    pub category: String,
    pub injection_on: u64,
    pub injection_off: u64,
    pub delta: i64,
}

impl<'a> AccountingRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn project_id(&self, evidence: &str) -> familiar_ai_core::Result<String> {
        self.conn
            .query_row(
                "SELECT project_id FROM project_registry_bindings WHERE evidence_value=?1 ORDER BY bound_at DESC, binding_id DESC LIMIT 1",
                [evidence],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?
            .ok_or_else(|| FamiliarError::Database("repository identity is not bound to a durable project".into()))
    }

    pub fn resolve_project(
        &self,
        evidence: Option<&str>,
    ) -> familiar_ai_core::Result<ProjectIdentityResolution> {
        let Some(evidence) = evidence else {
            return Ok(ProjectIdentityResolution::Degraded {
                evidence: None,
                reason: "git-metadata-unavailable".into(),
            });
        };
        Ok(match self.project_id(evidence) {
            Ok(project_id) => ProjectIdentityResolution::Resolved { project_id },
            Err(_) => ProjectIdentityResolution::Degraded {
                evidence: Some(evidence.into()),
                reason: "durable-project-unbound".into(),
            },
        })
    }

    pub fn register_project(
        &self,
        project_id: &str,
        name: &str,
        evidence_kind: &str,
        evidence: &str,
        actor: &str,
    ) -> familiar_ai_core::Result<()> {
        if !project_id.starts_with("prj_") || name.trim().is_empty() || actor.trim().is_empty() {
            return Err(FamiliarError::Database(
                "invalid durable project registration".into(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute("INSERT OR IGNORE INTO durable_projects(project_id,display_name,created_at) VALUES(?1,?2,?3)", params![project_id,name,now]).map_err(db)?;
        tx.execute("INSERT OR IGNORE INTO project_registry_bindings(binding_id,project_id,evidence_kind,evidence_value,actor,bound_at) VALUES(?1,?2,?3,?4,?5,?6)", params![format!("pbd_{}",random_hex()?),project_id,evidence_kind,evidence,actor,now]).map_err(db)?;
        tx.commit().map_err(db)
    }

    pub fn bind_provider(
        &self,
        project_id: &str,
        provider: &str,
        scope_kind: &str,
        scope_value: &str,
        confidence: &str,
        actor: &str,
    ) -> familiar_ai_core::Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO provider_attribution_bindings(binding_id,project_id,provider,scope_kind,scope_value,confidence,actor,bound_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![format!("pab_{}",random_hex()?),project_id,provider,scope_kind,scope_value,confidence,actor,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }

    /// Provider-neutral historical accounting query. Facts are assigned by
    /// period_start and are never prorated, even when their native interval
    /// crosses a requested bucket boundary.
    pub fn usage_series(
        &self,
        request: &UsageSeriesRequest,
    ) -> familiar_ai_core::Result<Vec<UsageSeriesPoint>> {
        if request.start >= request.end {
            return Err(FamiliarError::Database(
                "usage series range must be non-empty".into(),
            ));
        }
        const DIMENSIONS: &[&str] = &[
            "project",
            "provider",
            "model",
            "prd",
            "execution",
            "attempt",
            "stage",
            "billing_source",
            "attribution_status",
        ];
        if request
            .group_by
            .iter()
            .chain(request.filters.keys())
            .any(|v| !DIMENSIONS.contains(&v.as_str()))
        {
            return Err(FamiliarError::Database(
                "unsupported usage-series dimension".into(),
            ));
        }
        let mut facts = self.local_series_facts(request)?;
        facts.extend(self.openai_series_facts(request)?);
        facts.extend(self.anthropic_series_facts(request)?);
        facts.retain(|fact| {
            request
                .filters
                .iter()
                .all(|(key, value)| fact.dimensions.get(key) == Some(value))
        });

        let mut points: BTreeMap<SeriesKey, UsageSeriesPoint> = BTreeMap::new();
        for fact in facts {
            let start = bucket_start(fact.assignment, request.bucket);
            let grouped: Vec<_> = request
                .group_by
                .iter()
                .map(|key| {
                    (
                        key.clone(),
                        fact.dimensions
                            .get(key)
                            .cloned()
                            .unwrap_or_else(|| "unknown".into()),
                    )
                })
                .collect();
            let point =
                points
                    .entry((start, grouped.clone()))
                    .or_insert_with(|| UsageSeriesPoint {
                        bucket_start: start.to_rfc3339(),
                        bucket_end: next_bucket(start, request.bucket).to_rfc3339(),
                        dimensions: grouped.into_iter().collect(),
                        estimated_cost_nanousd: None,
                        authoritative_cost_nanousd: None,
                        input_tokens: None,
                        cached_input_tokens: None,
                        output_tokens: None,
                        reasoning_tokens: None,
                        request_count: None,
                        credit_estimate_microcredits: None,
                        observation_ids: Vec::new(),
                    });
            add_optional(&mut point.estimated_cost_nanousd, fact.estimated_cost);
            add_optional(
                &mut point.authoritative_cost_nanousd,
                fact.authoritative_cost,
            );
            add_optional(&mut point.input_tokens, fact.input_tokens);
            add_optional(&mut point.cached_input_tokens, fact.cached_input_tokens);
            add_optional(&mut point.output_tokens, fact.output_tokens);
            add_optional(&mut point.reasoning_tokens, fact.reasoning_tokens);
            add_optional(&mut point.request_count, fact.request_count);
            add_optional(
                &mut point.credit_estimate_microcredits,
                fact.credit_estimate,
            );
            point.observation_ids.push(fact.id);
        }
        if request.dense {
            let groups: BTreeSet<SeriesGroup> =
                points.keys().map(|(_, group)| group.clone()).collect();
            let mut cursor = bucket_start(request.start, request.bucket);
            while cursor < request.end {
                for group in &groups {
                    points
                        .entry((cursor, group.clone()))
                        .or_insert_with(|| UsageSeriesPoint {
                            bucket_start: cursor.to_rfc3339(),
                            bucket_end: next_bucket(cursor, request.bucket).to_rfc3339(),
                            dimensions: group.clone().into_iter().collect(),
                            estimated_cost_nanousd: Some(0),
                            authoritative_cost_nanousd: Some(0),
                            input_tokens: Some(0),
                            cached_input_tokens: Some(0),
                            output_tokens: Some(0),
                            reasoning_tokens: Some(0),
                            request_count: Some(0),
                            credit_estimate_microcredits: Some(0),
                            observation_ids: Vec::new(),
                        });
                }
                cursor = next_bucket(cursor, request.bucket);
            }
        }
        let mut result: Vec<_> = points.into_values().collect();
        for point in &mut result {
            point.observation_ids.sort();
            point.observation_ids.dedup();
        }
        Ok(result)
    }

    fn local_series_facts(
        &self,
        request: &UsageSeriesRequest,
    ) -> familiar_ai_core::Result<Vec<SeriesFact>> {
        let mut statement = self.conn.prepare("SELECT u.observation_id,u.period_start,coalesce((SELECT c.project_id FROM accounting_corrections c WHERE c.observation_id=u.observation_id AND c.correction_kind='reattribution' ORDER BY c.effective_at DESC,c.correction_id DESC LIMIT 1),u.project_id),u.degraded_identity,u.adapter,u.model_identity,e.prd_path,u.execution_id,u.attempt_id,u.stage,coalesce((SELECT sum(amount) FROM cost_estimates c WHERE c.observation_id=u.observation_id AND c.unit='nanoUSD'),NULL),u.uncached_input_tokens,u.cache_read_tokens,u.output_tokens,u.reasoning_output_tokens,(SELECT sum(amount_micocredits) FROM credit_estimates c WHERE c.observation_id=u.observation_id) FROM usage_observations u JOIN execution_history e ON e.execution_id=u.execution_id WHERE u.period_start>=?1 AND u.period_start<?2 ORDER BY u.period_start,u.observation_id").map_err(db)?;
        let rows = statement
            .query_map(
                params![request.start.to_rfc3339(), request.end.to_rfc3339()],
                |row| {
                    let project: Option<String> = row.get(2)?;
                    let degraded: Option<String> = row.get(3)?;
                    let mut dimensions = BTreeMap::new();
                    dimensions.insert(
                        "project".into(),
                        project.clone().unwrap_or_else(|| {
                            format!(
                                "degraded:{}",
                                degraded.unwrap_or_else(|| "unresolved".into())
                            )
                        }),
                    );
                    dimensions.insert("provider".into(), row.get::<_, String>(4)?);
                    dimensions.insert(
                        "model".into(),
                        row.get::<_, Option<String>>(5)?
                            .unwrap_or_else(|| "unknown".into()),
                    );
                    dimensions.insert("prd".into(), row.get(6)?);
                    dimensions.insert("execution".into(), row.get(7)?);
                    dimensions.insert("attempt".into(), row.get(8)?);
                    dimensions.insert("stage".into(), row.get(9)?);
                    dimensions.insert("billing_source".into(), "local".into());
                    dimensions.insert(
                        "attribution_status".into(),
                        if project.is_some() {
                            "attributed"
                        } else {
                            "degraded-identity"
                        }
                        .into(),
                    );
                    Ok(SeriesFact {
                        id: row.get(0)?,
                        assignment: parse_utc(row.get::<_, String>(1)?).map_err(sql_conversion)?,
                        dimensions,
                        estimated_cost: row.get(10)?,
                        authoritative_cost: None,
                        input_tokens: row.get(11)?,
                        cached_input_tokens: row.get(12)?,
                        output_tokens: row.get(13)?,
                        reasoning_tokens: row.get(14)?,
                        request_count: Some(1),
                        credit_estimate: row.get(15)?,
                    })
                },
            )
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    fn openai_series_facts(
        &self,
        request: &UsageSeriesRequest,
    ) -> familiar_ai_core::Result<Vec<SeriesFact>> {
        let mut statement=self.conn.prepare("SELECT r.revision_id,r.bucket_start,r.source_id,r.organization_id,r.project_id,r.amount_nanousd,(SELECT b.project_id FROM provider_attribution_bindings b WHERE b.provider='openai' AND ((b.scope_kind='project' AND b.scope_value=r.project_id) OR (b.scope_kind='organization' AND b.scope_value=r.organization_id)) ORDER BY CASE b.scope_kind WHEN 'project' THEN 0 ELSE 1 END,b.bound_at DESC LIMIT 1) FROM openai_cost_revisions r LEFT JOIN openai_cost_revisions newer ON newer.supersedes_revision_id=r.revision_id WHERE newer.revision_id IS NULL AND r.bucket_start>=?1 AND r.bucket_start<?2 AND r.amount_nanousd IS NOT NULL ORDER BY r.bucket_start,r.revision_id").map_err(db)?;
        let rows = statement
            .query_map(
                params![request.start.timestamp(), request.end.timestamp()],
                |row| {
                    provider_fact(
                        row,
                        "openai",
                        DateTime::from_timestamp(row.get(1)?, 0)
                            .ok_or(rusqlite::Error::InvalidQuery)?,
                    )
                },
            )
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    fn anthropic_series_facts(
        &self,
        request: &UsageSeriesRequest,
    ) -> familiar_ai_core::Result<Vec<SeriesFact>> {
        let mut statement=self.conn.prepare("SELECT r.revision_id,r.bucket_start,r.source_name,'',r.workspace_id,r.amount_nanousd,b.project_id FROM current_provider_costs r LEFT JOIN provider_attribution_bindings b ON b.provider='anthropic' AND b.scope_kind='workspace' AND b.scope_value=r.workspace_id WHERE r.bucket_start>=?1 AND r.bucket_start<?2 ORDER BY r.bucket_start,r.revision_id").map_err(db)?;
        let rows = statement
            .query_map(
                params![request.start.to_rfc3339(), request.end.to_rfc3339()],
                |row| {
                    let time = parse_utc(row.get::<_, String>(1)?).map_err(sql_conversion)?;
                    provider_fact(row, "anthropic", time)
                },
            )
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    pub fn append_reattribution(
        &self,
        observation_id: &str,
        project_id: &str,
        actor: &str,
        reason: &str,
        effective_at: &str,
    ) -> familiar_ai_core::Result<String> {
        let prior: Option<String>=self.conn.query_row("SELECT coalesce((SELECT project_id FROM accounting_corrections WHERE observation_id=?1 AND correction_kind='reattribution' ORDER BY effective_at DESC,correction_id DESC LIMIT 1),(SELECT project_id FROM usage_observations WHERE observation_id=?1))",[observation_id],|r|r.get(0)).optional().map_err(db)?.flatten();
        let id = format!("cor_{}", random_hex()?);
        self.conn.execute("INSERT INTO accounting_corrections(correction_id,observation_id,correction_kind,prior_project_id,project_id,reason,actor,effective_at) VALUES(?1,?2,'reattribution',?3,?4,?5,?6,?7)",params![id,observation_id,prior,project_id,reason,actor,effective_at]).map_err(db)?;
        Ok(id)
    }

    /// Atomically replaces disposable rollups; raw observations are untouched.
    pub fn rebuild_rollups(
        &self,
        request: &UsageSeriesRequest,
        definition_version: i64,
    ) -> familiar_ai_core::Result<usize> {
        let points = self.usage_series(request)?;
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute(
            "DELETE FROM usage_rollups WHERE definition_version=?1 AND bucket_kind=?2",
            params![definition_version, bucket_name(request.bucket)],
        )
        .map_err(db)?;
        for point in &points {
            let dimensions = serde_json::to_string(&point.dimensions)
                .map_err(|e| FamiliarError::Database(e.to_string()))?;
            let metrics =
                serde_json::to_string(point).map_err(|e| FamiliarError::Database(e.to_string()))?;
            let ids = serde_json::to_string(&point.observation_ids)
                .map_err(|e| FamiliarError::Database(e.to_string()))?;
            tx.execute("INSERT INTO usage_rollups(definition_version,bucket_kind,bucket_start,dimensions_json,metrics_json,observation_ids_json,rebuilt_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![definition_version,bucket_name(request.bucket),point.bucket_start,dimensions,metrics,ids,now]).map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(points.len())
    }

    /// Idempotency is keyed by the sanitized source-event hash. Evidence and
    /// its observation commit atomically; replay returns the existing
    /// observation so a missing idempotent cost fact can still be repaired.
    pub fn append_observation(
        &self,
        value: &UsageObservation<'_>,
    ) -> familiar_ai_core::Result<Option<String>> {
        if let Some(existing) = self
            .conn
            .query_row(
                "SELECT observation_id FROM usage_observations observation JOIN accounting_evidence evidence ON evidence.evidence_id=observation.evidence_id WHERE evidence.source_event_hash=?1",
                [value.source_event_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?
        {
            return Ok(Some(existing));
        }
        let evidence_id = format!("evi_{}", random_hex()?);
        let observation_id = format!("obs_{}", random_hex()?);
        let now = Utc::now().to_rfc3339();
        let (project_id, degraded) =
            match self.resolve_project(value.project_resolution_evidence)? {
                ProjectIdentityResolution::Resolved { project_id } => (Some(project_id), None),
                ProjectIdentityResolution::Degraded { reason, .. } => (None, Some(reason)),
            };
        let usage_json = serde_json::json!({"uncached_input_tokens":value.uncached_input_tokens,"cache_read_tokens":value.cache_read_tokens,"cache_write_tokens":value.cache_write_tokens,"output_tokens":value.output_tokens,"reasoning_output_tokens":value.reasoning_output_tokens}).to_string();
        let selected_spec: Option<(Option<String>, Option<String>)> = self.conn.query_row(
            "SELECT selected_spec_identity,selected_empirical_version FROM worker_selections WHERE execution_id=?1 AND stage=?2 ORDER BY recorded_at DESC LIMIT 1",
            params![value.execution_id, value.stage], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(db)?;
        let (spec_identity, empirical_version) = selected_spec.unwrap_or((None, None));
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute("INSERT INTO accounting_evidence(evidence_id,execution_id,adapter,cli_version,model_identity,provider_session_id,provider_request_id,usage_json,provider_cost_lexical,observed_at,terminal_status,source_event_hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)", params![evidence_id,value.execution_id,value.adapter,value.cli_version,value.model_identity,value.session_id,value.provider_request_id,usage_json,value.provider_cost_lexical,value.period_end,value.terminal_status,value.source_event_hash]).map_err(db)?;
        tx.execute("INSERT INTO usage_observations(observation_id,evidence_id,project_id,degraded_identity,execution_id,attempt_id,stage,session_id,worker_identity,adapter,model_identity,service_tier,provider_request_id,uncached_input_tokens,cache_read_tokens,cache_write_tokens,output_tokens,reasoning_output_tokens,unknown_reason,period_start,period_end,observed_at,ingested_at,spec_identity,empirical_version,output_register_id,output_register_version,input_compression_id,input_compression_version,compression_experiment,compression_lane) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30)", params![observation_id,evidence_id,project_id,degraded,value.execution_id,value.attempt_id,value.stage,value.session_id,value.worker_identity,value.adapter,value.model_identity,value.service_tier,value.provider_request_id,value.uncached_input_tokens,value.cache_read_tokens,value.cache_write_tokens,value.output_tokens,value.reasoning_output_tokens,value.unknown_reason,value.period_start,value.period_end,now,spec_identity,empirical_version,value.output_register_id,value.output_register_version,value.input_compression_id,value.input_compression_version,value.compression_experiment,value.compression_lane]).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(Some(observation_id))
    }

    pub fn append_vendor_estimate(
        &self,
        observation_id: &str,
        lexical_usd: &str,
    ) -> familiar_ai_core::Result<()> {
        let amount = decimal_nanousd(lexical_usd)
            .ok_or_else(|| FamiliarError::Database("invalid provider USD lexical amount".into()))?;
        self.conn.execute("INSERT OR IGNORE INTO cost_estimates(estimate_id,observation_id,billing_mode,provenance,unit,amount,lexical_amount,created_at) VALUES(?1,?2,'local-estimate','vendor-reported','nanoUSD',?3,?4,?5)", params![format!("est_{}",random_hex()?),observation_id,amount,lexical_usd,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }

    pub fn append_legacy_configured_estimate(
        &self,
        observation_id: &str,
        model: &str,
        amount_microusd: u64,
        rates_json: &str,
    ) -> familiar_ai_core::Result<()> {
        let schedule_id = "legacy-execution-history-pricing";
        let now = Utc::now().to_rfc3339();
        self.conn.execute("INSERT OR IGNORE INTO price_schedules(schedule_id,effective_at,currency,calculation_version,rates_json,created_at) VALUES(?1,'legacy','USD','legacy-microusd-v1',?2,?3)", params![schedule_id,rates_json,now]).map_err(db)?;
        let amount = amount_microusd
            .checked_mul(1000)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or_else(|| FamiliarError::Database("nanoUSD arithmetic overflow".into()))?;
        self.conn.execute("INSERT OR IGNORE INTO cost_estimates(estimate_id,observation_id,billing_mode,provenance,unit,amount,schedule_id,calculation_version,lexical_amount,created_at) VALUES(?1,?2,'local-estimate','configured-rate','nanoUSD',?3,?4,'legacy-microusd-v1',?5,?6)", params![format!("est_{}",random_hex()?),observation_id,amount,schedule_id,format!("model={model};microusd={amount_microusd}"),now]).map_err(db)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_credit_estimate(
        &self,
        observation_id: &str,
        schedule_id: &str,
        source_url: &str,
        effective_at: &str,
        calculation_version: &str,
        rates_json: &str,
        amount_micocredits: u64,
    ) -> familiar_ai_core::Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute("INSERT OR IGNORE INTO credit_schedules(schedule_id,source_url,effective_at,calculation_version,rates_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![schedule_id,source_url,effective_at,calculation_version,rates_json,now]).map_err(db)?;
        self.conn.execute("INSERT OR IGNORE INTO credit_estimates(estimate_id,observation_id,schedule_id,amount_micocredits,calculation_version,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![format!("cre_{}",random_hex()?),observation_id,schedule_id,amount_micocredits,calculation_version,now]).map_err(db)?;
        Ok(())
    }

    /// Appends a changed provider fact and links it to the prior effective
    /// revision. An identical payload is an idempotent no-op.
    pub fn append_openai_cost_revision(
        &self,
        fact: &OpenAiCostFact<'_>,
    ) -> familiar_ai_core::Result<bool> {
        let duplicate: Option<String> = self.conn.query_row("SELECT revision_id FROM openai_cost_revisions WHERE source_id=?1 AND organization_id=?2 AND project_id IS ?3 AND bucket_start=?4 AND bucket_end=?5 AND line_item=?6 AND payload_hash=?7", params![fact.source_id,fact.organization_id,fact.project_id,fact.bucket_start,fact.bucket_end,fact.line_item,fact.payload_hash], |r| r.get(0)).optional().map_err(db)?;
        if duplicate.is_some() {
            return Ok(false);
        }
        let prior: Option<String> = self.conn.query_row("SELECT revision_id FROM openai_cost_revisions WHERE source_id=?1 AND organization_id=?2 AND project_id IS ?3 AND bucket_start=?4 AND bucket_end=?5 AND line_item=?6 ORDER BY collected_at DESC, rowid DESC LIMIT 1", params![fact.source_id,fact.organization_id,fact.project_id,fact.bucket_start,fact.bucket_end,fact.line_item], |r| r.get(0)).optional().map_err(db)?;
        let (amount, normalization_error) = if fact.currency.eq_ignore_ascii_case("usd") {
            (
                Some(
                    signed_decimal_nanousd(fact.raw_amount_lexical).ok_or_else(|| {
                        FamiliarError::Database("invalid OpenAI USD decimal".into())
                    })?,
                ),
                None,
            )
        } else {
            (
                None,
                Some("non-USD provider cost retained without conversion"),
            )
        };
        self.conn.execute("INSERT INTO openai_cost_revisions(revision_id,source_id,organization_id,project_id,bucket_start,bucket_end,line_item,classification,raw_amount_lexical,currency,amount_nanousd,normalization_error,payload_hash,supersedes_revision_id,collected_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,lower(?10),?11,?12,?13,?14,?15)", params![format!("ocr_{}",random_hex()?),fact.source_id,fact.organization_id,fact.project_id,fact.bucket_start,fact.bucket_end,fact.line_item,fact.classification,fact.raw_amount_lexical,fact.currency,amount,normalization_error,fact.payload_hash,prior,fact.collected_at]).map_err(db)?;
        Ok(true)
    }

    pub fn usage(&self) -> familiar_ai_core::Result<LedgerUsageSummary> {
        let tokens = self.conn.query_row("SELECT count(*),coalesce(sum(CASE WHEN unknown_reason IS NOT NULL THEN 1 ELSE 0 END),0),coalesce(sum(uncached_input_tokens),0),coalesce(sum(cache_read_tokens),0),coalesce(sum(cache_write_tokens),0),coalesce(sum(output_tokens),0),coalesce(sum(reasoning_output_tokens),0) FROM usage_observations", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).map_err(db)?;
        let costs = self.conn.query_row("SELECT coalesce(sum(CASE WHEN unit='nanoUSD' THEN amount ELSE 0 END),0),coalesce(sum(CASE WHEN provenance='vendor-reported' THEN 1 ELSE 0 END),0),coalesce(sum(CASE WHEN provenance='configured-rate' THEN 1 ELSE 0 END),0),coalesce(sum(CASE WHEN provenance='known-zero' THEN 1 ELSE 0 END),0) FROM cost_estimates", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(db)?;
        Ok(LedgerUsageSummary {
            observations: tokens.0,
            unknown_observations: tokens.1,
            uncached_input_tokens: tokens.2,
            cache_read_tokens: tokens.3,
            cache_write_tokens: tokens.4,
            output_tokens: tokens.5,
            reasoning_output_tokens: tokens.6,
            known_nanousd: costs.0,
            vendor_reported_estimates: costs.1,
            configured_rate_estimates: costs.2,
            known_zero_estimates: costs.3,
        })
    }

    pub fn compression_experiment(
        &self,
        label: &str,
    ) -> familiar_ai_core::Result<CompressionExperimentSummary> {
        Ok(CompressionExperimentSummary {
            off: self.compression_lane(label, "off")?,
            on: self.compression_lane(label, "on")?,
        })
    }

    fn compression_lane(
        &self,
        label: &str,
        lane: &str,
    ) -> familiar_ai_core::Result<CompressionLaneSummary> {
        self.conn.query_row(
            "SELECT count(*),sum(uncached_input_tokens),sum(cache_read_tokens),sum(cache_write_tokens),sum(output_tokens),sum((SELECT amount FROM cost_estimates c WHERE c.observation_id=o.observation_id AND c.unit='nanoUSD' LIMIT 1)) FROM usage_observations o WHERE compression_experiment=?1 AND compression_lane=?2",
            params![label, lane],
            |row| Ok(CompressionLaneSummary { observations: row.get(0)?, uncached_input_tokens: row.get(1)?, cache_read_tokens: row.get(2)?, cache_write_tokens: row.get(3)?, output_tokens: row.get(4)?, known_nanousd: row.get(5)? }),
        ).map_err(db)
    }

    pub fn token_sinks(&self, project_id: &str) -> familiar_ai_core::Result<Vec<TokenSink>> {
        let dimensions = [("stage", "stage"), ("worker", "worker_identity")];
        let categories = [
            ("uncached_input", "uncached_input_tokens"),
            ("cache_read", "cache_read_tokens"),
            ("cache_write", "cache_write_tokens"),
            ("output", "output_tokens"),
            ("reasoning_output", "reasoning_output_tokens"),
        ];
        let mut out = Vec::new();
        for (dimension, column) in dimensions {
            for (category, token_column) in categories {
                let sql=format!("SELECT {column},sum({token_column}) FROM usage_observations WHERE project_id=?1 AND {token_column} IS NOT NULL GROUP BY {column}");
                let mut statement = self.conn.prepare(&sql).map_err(db)?;
                let rows = statement
                    .query_map([project_id], |r| {
                        Ok(TokenSink {
                            dimension: dimension.into(),
                            value: r.get(0)?,
                            category: category.into(),
                            tokens: r.get(1)?,
                        })
                    })
                    .map_err(db)?;
                for row in rows {
                    out.push(row.map_err(db)?);
                }
            }
        }
        out.sort_by_key(|v| {
            (
                std::cmp::Reverse(v.tokens),
                v.dimension.clone(),
                v.value.clone(),
                v.category.clone(),
            )
        });
        Ok(out)
    }
    pub fn context_effect(&self, project_id: &str) -> familiar_ai_core::Result<Vec<ContextEffect>> {
        let categories = [
            ("uncached_input", "uncached_input_tokens"),
            ("cache_read", "cache_read_tokens"),
            ("cache_write", "cache_write_tokens"),
            ("output", "output_tokens"),
            ("reasoning_output", "reasoning_output_tokens"),
        ];
        let mut out = Vec::new();
        for (category, column) in categories {
            let sql=format!("SELECT coalesce(sum(CASE WHEN c.injection_enabled=1 THEN u.{column} END),0),coalesce(sum(CASE WHEN c.injection_enabled=0 THEN u.{column} END),0) FROM usage_observations u JOIN context_service_executions c ON c.execution_id=u.execution_id WHERE u.project_id=?1 AND u.{column} IS NOT NULL");
            let (on, off): (u64, u64) = self
                .conn
                .query_row(&sql, [project_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(db)?;
            out.push(ContextEffect {
                category: category.into(),
                injection_on: on,
                injection_off: off,
                delta: i64::try_from(on)
                    .unwrap_or(i64::MAX)
                    .saturating_sub(i64::try_from(off).unwrap_or(i64::MAX)),
            });
        }
        Ok(out)
    }

    /// Deterministic cost reconciliation (PRD-053). Compares the
    /// current-effective provider-authoritative projection against locally
    /// attributed estimates, one row per UTC day per (project or
    /// `unattributed`) within `[window_start, window_end)`. Rows are new
    /// facts: raw provider revisions and local estimates are never edited.
    /// Re-running an unchanged window inserts nothing (`rows_unchanged`
    /// grows instead); a changed local estimate or a superseding provider
    /// revision appends a new row linked via `supersedes_row_id` to the
    /// prior current-effective row for that key, which remains as history.
    #[allow(clippy::too_many_arguments)]
    pub fn reconcile_window(
        &self,
        billing_source: &str,
        window_start: DateTime<Utc>,
        window_end: DateTime<Utc>,
        invoked_by: &str,
        tolerance_nanousd: i64,
        settlement_horizon_days: i64,
        now: DateTime<Utc>,
        actor: &str,
    ) -> familiar_ai_core::Result<ReconciliationSummary> {
        if window_start >= window_end {
            return Err(FamiliarError::Database(
                "reconciliation window must be non-empty".into(),
            ));
        }
        if invoked_by != "collect" && invoked_by != "explicit" {
            return Err(FamiliarError::Database(
                "reconciliation invocation must be 'collect' or 'explicit'".into(),
            ));
        }
        if tolerance_nanousd < 0 || settlement_horizon_days < 0 {
            return Err(FamiliarError::Database(
                "reconciliation tolerance and settlement horizon must be non-negative".into(),
            ));
        }
        let run_id = format!("recr_{}", random_hex()?);
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute(
            "INSERT INTO reconciliation_runs(run_id,billing_source,window_start,window_end,invoked_by,tolerance_nanousd,settlement_horizon_days,actor,started_at,now_reference) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![run_id,billing_source,window_start.to_rfc3339(),window_end.to_rfc3339(),invoked_by,tolerance_nanousd,settlement_horizon_days,actor,Utc::now().to_rfc3339(),now.to_rfc3339()],
        ).map_err(db)?;

        let mut rows_appended = 0usize;
        let mut rows_unchanged = 0usize;
        let mut rows = Vec::new();
        let mut day = bucket_start(window_start, UsageBucket::Day);
        while day < window_end {
            let day_end = next_bucket(day, UsageBucket::Day);
            // Authoritative side: current-effective provider revisions for
            // this source and day, resolved to a project via the PRD-055
            // workspace attribution binding where one exists.
            let mut authoritative: BTreeMap<Option<String>, (i64, Vec<String>)> = BTreeMap::new();
            {
                let mut statement = tx.prepare("SELECT r.revision_id,r.amount_nanousd,b.project_id FROM current_provider_costs r LEFT JOIN provider_attribution_bindings b ON b.provider=?1 AND b.scope_kind='workspace' AND b.scope_value=r.workspace_id WHERE r.source_name=?1 AND r.bucket_start>=?2 AND r.bucket_start<?3").map_err(db)?;
                let found = statement
                    .query_map(
                        params![billing_source, day.to_rfc3339(), day_end.to_rfc3339()],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, Option<String>>(2)?,
                            ))
                        },
                    )
                    .map_err(db)?;
                for entry in found {
                    let (revision_id, amount, project) = entry.map_err(db)?;
                    let slot = authoritative.entry(project).or_insert((0, Vec::new()));
                    slot.0 = slot.0.saturating_add(amount);
                    slot.1.push(revision_id);
                }
            }
            // Local side: local-estimate cost_estimates for observations
            // covered by this day, resolved to a project (reattribution
            // corrections take priority over the observation's original
            // binding, matching `local_series_facts`). Degraded-identity
            // observations have no project and are excluded from matching.
            let mut local: BTreeMap<String, (i64, Vec<String>)> = BTreeMap::new();
            {
                let mut statement = tx.prepare("SELECT u.observation_id,coalesce((SELECT c.project_id FROM accounting_corrections c WHERE c.observation_id=u.observation_id AND c.correction_kind='reattribution' ORDER BY c.effective_at DESC,c.correction_id DESC LIMIT 1),u.project_id),(SELECT sum(amount) FROM cost_estimates c WHERE c.observation_id=u.observation_id AND c.unit='nanoUSD' AND c.billing_mode='local-estimate') FROM usage_observations u WHERE u.period_start>=?1 AND u.period_start<?2").map_err(db)?;
                let found = statement
                    .query_map(params![day.to_rfc3339(), day_end.to_rfc3339()], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                        ))
                    })
                    .map_err(db)?;
                for entry in found {
                    let (observation_id, project, amount) = entry.map_err(db)?;
                    let (Some(project), Some(amount)) = (project, amount) else {
                        continue;
                    };
                    let slot = local.entry(project).or_insert((0, Vec::new()));
                    slot.0 = slot.0.saturating_add(amount);
                    slot.1.push(observation_id);
                }
            }
            let mut projects: BTreeSet<String> = BTreeSet::new();
            for key in authoritative.keys().flatten() {
                projects.insert(key.clone());
            }
            for key in local.keys() {
                projects.insert(key.clone());
            }
            // Provider spend never traced to a project: explicit visible
            // unattributed spend, never an error, never distributed.
            type Candidate = (
                String,
                Option<String>,
                Option<i64>,
                Option<i64>,
                Vec<String>,
                Vec<String>,
            );
            let mut candidates: Vec<Candidate> = Vec::new();
            if let Some((amount, revisions)) = authoritative.get(&None) {
                if !revisions.is_empty() {
                    candidates.push((
                        "unattributed".into(),
                        None,
                        None,
                        Some(*amount),
                        revisions.clone(),
                        Vec::new(),
                    ));
                }
            }
            for project in projects {
                let auth = authoritative.get(&Some(project.clone()));
                let loc = local.get(&project);
                candidates.push((
                    format!("project:{project}"),
                    Some(project),
                    loc.map(|v| v.0),
                    auth.map(|v| v.0),
                    auth.map(|v| v.1.clone()).unwrap_or_default(),
                    loc.map(|v| v.1.clone()).unwrap_or_default(),
                ));
            }
            for (
                match_key,
                project_id,
                local_amount,
                authoritative_amount,
                provider_revision_ids,
                observation_ids,
            ) in candidates
            {
                let (status, variance) = classify_reconciliation(
                    local_amount,
                    authoritative_amount,
                    tolerance_nanousd,
                    day_end,
                    settlement_horizon_days,
                    now,
                );
                let (reservation_count, reservation_amount) = if let Some(project) = &project_id {
                    reservation_evidence(&tx, project, day, day_end)?
                } else {
                    (0, None)
                };
                type ExistingRow = (String, String, Option<i64>, Option<i64>, Option<i64>, i64);
                let existing: Option<ExistingRow> = tx
                    .query_row(
                        "SELECT row_id,status,local_estimate_nanousd,authoritative_nanousd,variance_nanousd,tolerance_nanousd FROM current_reconciliation WHERE billing_source=?1 AND day_start=?2 AND match_key=?3",
                        params![billing_source, day.to_rfc3339(), match_key],
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
                    .optional()
                    .map_err(db)?;
                let unchanged = existing.as_ref().is_some_and(
                    |(
                        _,
                        existing_status,
                        existing_local,
                        existing_auth,
                        existing_variance,
                        existing_tolerance,
                    )| {
                        existing_status == status.as_str()
                            && *existing_local == local_amount
                            && *existing_auth == authoritative_amount
                            && *existing_variance == variance
                            && *existing_tolerance == tolerance_nanousd
                    },
                );
                if unchanged {
                    rows_unchanged += 1;
                    if let Some((row_id, ..)) = existing {
                        rows.push(fetch_reconciliation_row(&tx, &row_id)?);
                    }
                    continue;
                }
                let row_id = format!("recw_{}", random_hex()?);
                let created_at = Utc::now().to_rfc3339();
                tx.execute(
                    "INSERT INTO reconciliation_rows(row_id,run_id,billing_source,day_start,day_end,match_key,project_id,status,local_estimate_nanousd,authoritative_nanousd,variance_nanousd,tolerance_nanousd,provider_revision_ids,observation_ids,reservation_evidence_count,reservation_evidence_nanousd,supersedes_row_id,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                    params![
                        row_id,
                        run_id,
                        billing_source,
                        day.to_rfc3339(),
                        day_end.to_rfc3339(),
                        match_key,
                        project_id,
                        status.as_str(),
                        local_amount,
                        authoritative_amount,
                        variance,
                        tolerance_nanousd,
                        serde_json::to_string(&provider_revision_ids)
                            .map_err(|e| FamiliarError::Database(e.to_string()))?,
                        serde_json::to_string(&observation_ids)
                            .map_err(|e| FamiliarError::Database(e.to_string()))?,
                        reservation_count,
                        reservation_amount,
                        existing.map(|(row_id, ..)| row_id),
                        created_at,
                    ],
                )
                .map_err(db)?;
                rows_appended += 1;
                rows.push(fetch_reconciliation_row(&tx, &row_id)?);
            }
            day = day_end;
        }
        tx.commit().map_err(db)?;
        Ok(ReconciliationSummary {
            run_id,
            rows_appended,
            rows_unchanged,
            rows,
        })
    }

    /// Current-effective reconciliation rows for one durable project,
    /// cached-only and read-only — never a network call.
    pub fn reconciliation_for_project(
        &self,
        project_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> familiar_ai_core::Result<Vec<ReconciliationRow>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {RECONCILIATION_COLUMNS} FROM current_reconciliation WHERE project_id=?1 AND day_start>=?2 AND day_start<?3 ORDER BY day_start,billing_source"
        )).map_err(db)?;
        let rows = statement
            .query_map(
                params![project_id, start.to_rfc3339(), end.to_rfc3339()],
                map_reconciliation_row,
            )
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    /// Month-to-date cost per billing source, computed only from the
    /// current-effective reconciliation projection — cached-only, no
    /// network. `now` bounds the "to-date" cutoff and must be caller-supplied
    /// for determinism.
    pub fn month_to_date_by_source(
        &self,
        now: DateTime<Utc>,
    ) -> familiar_ai_core::Result<Vec<SourceMonthSummary>> {
        let month_start = Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .unwrap();
        let mut statement = self.conn.prepare("SELECT billing_source,status,local_estimate_nanousd,authoritative_nanousd,created_at FROM current_reconciliation WHERE day_start>=?1 AND day_start<?2 ORDER BY billing_source,day_start").map_err(db)?;
        let rows = statement
            .query_map(params![month_start.to_rfc3339(), now.to_rfc3339()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(db)?;
        let mut by_source: BTreeMap<String, SourceMonthSummary> = BTreeMap::new();
        for entry in rows {
            let (source, status, local, authoritative, created_at) = entry.map_err(db)?;
            let summary = by_source
                .entry(source.clone())
                .or_insert_with(|| SourceMonthSummary {
                    billing_source: source.clone(),
                    coverage_start: month_start.to_rfc3339(),
                    coverage_end: now.to_rfc3339(),
                    authoritative_nanousd: None,
                    local_estimate_nanousd: None,
                    unattributed_nanousd: None,
                    reconciled_days: 0,
                    reconciled_with_variance_days: 0,
                    pending_days: 0,
                    mismatch_days: 0,
                    unattributed_provider_spend_days: 0,
                    completeness: "complete".into(),
                    freshness: created_at.clone(),
                });
            if created_at > summary.freshness {
                summary.freshness = created_at;
            }
            match ReconciliationStatus::parse(&status) {
                Some(ReconciliationStatus::UnattributedProviderSpend) => {
                    summary.unattributed_provider_spend_days += 1;
                    add_optional(&mut summary.unattributed_nanousd, authoritative);
                }
                Some(ReconciliationStatus::Reconciled) => {
                    summary.reconciled_days += 1;
                    add_optional(&mut summary.authoritative_nanousd, authoritative);
                    add_optional(&mut summary.local_estimate_nanousd, local);
                }
                Some(ReconciliationStatus::ReconciledWithVariance) => {
                    summary.reconciled_with_variance_days += 1;
                    add_optional(&mut summary.authoritative_nanousd, authoritative);
                    add_optional(&mut summary.local_estimate_nanousd, local);
                }
                Some(ReconciliationStatus::Pending) => {
                    summary.pending_days += 1;
                    add_optional(&mut summary.local_estimate_nanousd, local);
                    summary.completeness = "incomplete".into();
                }
                Some(ReconciliationStatus::Mismatch) => {
                    summary.mismatch_days += 1;
                    add_optional(&mut summary.authoritative_nanousd, authoritative);
                    add_optional(&mut summary.local_estimate_nanousd, local);
                    summary.completeness = "incomplete".into();
                }
                None => {}
            }
        }
        Ok(by_source.into_values().collect())
    }

    /// Month-to-date per billing source plus an optional aggregate that
    /// preserves source attribution — the aggregate total travels alongside
    /// the per-source breakdown, never in place of it, so a caller can never
    /// lose which source contributed what.
    pub fn month_to_date_report(
        &self,
        now: DateTime<Utc>,
    ) -> familiar_ai_core::Result<MonthToDateReport> {
        let sources = self.month_to_date_by_source(now)?;
        let month_start = Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .unwrap();
        let mut aggregate = AggregateMonthSummary {
            coverage_start: month_start.to_rfc3339(),
            coverage_end: now.to_rfc3339(),
            source_count: sources.len(),
            ..Default::default()
        };
        for source in &sources {
            add_optional(
                &mut aggregate.authoritative_nanousd,
                source.authoritative_nanousd,
            );
            add_optional(
                &mut aggregate.local_estimate_nanousd,
                source.local_estimate_nanousd,
            );
            add_optional(
                &mut aggregate.unattributed_nanousd,
                source.unattributed_nanousd,
            );
        }
        Ok(MonthToDateReport { sources, aggregate })
    }

    /// PRD-032 scoring input: known local-estimate cost per (PRD, worker).
    /// `authority` is `"unknown"` when no observation for that PRD/worker
    /// has any nanoUSD cost_estimates row at all — that state must never be
    /// treated as free by a caller ranking workers by cost.
    pub fn accepted_prd_cost(&self) -> familiar_ai_core::Result<Vec<PrdCostScoreInput>> {
        let mut statement = self.conn.prepare("SELECT e.prd_path,u.worker_identity,sum(CASE WHEN EXISTS(SELECT 1 FROM cost_estimates c WHERE c.observation_id=u.observation_id AND c.unit='nanoUSD' AND c.billing_mode='local-estimate') THEN coalesce((SELECT sum(amount) FROM cost_estimates c WHERE c.observation_id=u.observation_id AND c.unit='nanoUSD' AND c.billing_mode='local-estimate'),0) ELSE 0 END),sum(CASE WHEN NOT EXISTS(SELECT 1 FROM cost_estimates c WHERE c.observation_id=u.observation_id AND c.unit='nanoUSD' AND c.billing_mode='local-estimate') THEN 1 ELSE 0 END),count(*) FROM usage_observations u JOIN execution_history e ON e.execution_id=u.execution_id GROUP BY e.prd_path,u.worker_identity ORDER BY e.prd_path,u.worker_identity").map_err(db)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(db)?;
        let mut out = Vec::new();
        for entry in rows {
            let (prd, worker_identity, known_total, unknown_count, total_count) =
                entry.map_err(db)?;
            let all_unknown = unknown_count == total_count;
            out.push(PrdCostScoreInput {
                prd,
                worker_identity,
                local_estimate_nanousd: if all_unknown { None } else { Some(known_total) },
                authority: if all_unknown { "unknown" } else { "estimated" }.into(),
                completeness: if unknown_count > 0 {
                    "incomplete"
                } else {
                    "complete"
                }
                .into(),
            });
        }
        Ok(out)
    }
}

fn random_hex() -> familiar_ai_core::Result<String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| FamiliarError::Database("secure project-id generation failed".into()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

struct SeriesFact {
    id: String,
    assignment: DateTime<Utc>,
    dimensions: BTreeMap<String, String>,
    estimated_cost: Option<i64>,
    authoritative_cost: Option<i64>,
    input_tokens: Option<i64>,
    cached_input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    request_count: Option<i64>,
    credit_estimate: Option<i64>,
}

fn provider_fact(
    row: &rusqlite::Row<'_>,
    provider: &str,
    assignment: DateTime<Utc>,
) -> rusqlite::Result<SeriesFact> {
    let project: Option<String> = row.get(6)?;
    let mut dimensions = BTreeMap::new();
    dimensions.insert(
        "project".into(),
        project.clone().unwrap_or_else(|| "unattributed".into()),
    );
    dimensions.insert("provider".into(), provider.into());
    dimensions.insert("model".into(), "unknown".into());
    dimensions.insert("prd".into(), "unknown".into());
    dimensions.insert("execution".into(), "unknown".into());
    dimensions.insert("attempt".into(), "unknown".into());
    dimensions.insert("stage".into(), "unknown".into());
    dimensions.insert("billing_source".into(), row.get(2)?);
    dimensions.insert(
        "attribution_status".into(),
        if project.is_some() {
            "attributed"
        } else {
            "unattributed"
        }
        .into(),
    );
    Ok(SeriesFact {
        id: row.get(0)?,
        assignment,
        dimensions,
        estimated_cost: None,
        authoritative_cost: row.get(5)?,
        input_tokens: None,
        cached_input_tokens: None,
        output_tokens: None,
        reasoning_tokens: None,
        request_count: None,
        credit_estimate: None,
    })
}

fn parse_utc(value: String) -> Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(&value).map(|value| value.with_timezone(&Utc))
}

fn sql_conversion(error: chrono::ParseError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn add_optional(target: &mut Option<i64>, value: Option<i64>) {
    if let Some(value) = value {
        *target = Some(target.unwrap_or(0).saturating_add(value));
    }
}

fn bucket_start(value: DateTime<Utc>, bucket: UsageBucket) -> DateTime<Utc> {
    match bucket {
        UsageBucket::Hour => value
            .with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap(),
        UsageBucket::Day => Utc
            .with_ymd_and_hms(value.year(), value.month(), value.day(), 0, 0, 0)
            .unwrap(),
        UsageBucket::Week => {
            let day = Utc
                .with_ymd_and_hms(value.year(), value.month(), value.day(), 0, 0, 0)
                .unwrap();
            day - Duration::days(value.weekday().num_days_from_monday().into())
        }
        UsageBucket::Month => Utc
            .with_ymd_and_hms(value.year(), value.month(), 1, 0, 0, 0)
            .unwrap(),
    }
}

fn next_bucket(value: DateTime<Utc>, bucket: UsageBucket) -> DateTime<Utc> {
    match bucket {
        UsageBucket::Hour => value + Duration::hours(1),
        UsageBucket::Day => value + Duration::days(1),
        UsageBucket::Week => value + Duration::weeks(1),
        UsageBucket::Month => {
            if value.month() == 12 {
                Utc.with_ymd_and_hms(value.year() + 1, 1, 1, 0, 0, 0)
                    .unwrap()
            } else {
                Utc.with_ymd_and_hms(value.year(), value.month() + 1, 1, 0, 0, 0)
                    .unwrap()
            }
        }
    }
}
fn bucket_name(bucket: UsageBucket) -> &'static str {
    match bucket {
        UsageBucket::Hour => "hour",
        UsageBucket::Day => "day",
        UsageBucket::Week => "week",
        UsageBucket::Month => "month",
    }
}
pub fn decimal_nanousd(value: &str) -> Option<u64> {
    let (whole, frac) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut n = whole.parse::<u64>().ok()?.checked_mul(1_000_000_000)?;
    let kept = &frac[..frac.len().min(9)];
    if !kept.is_empty() {
        n = n.checked_add(
            kept.parse::<u64>()
                .ok()?
                .checked_mul(10_u64.pow((9 - kept.len()) as u32))?,
        )?;
    }
    let round = match frac.as_bytes().get(9) {
        None | Some(b'0'..=b'4') => false,
        Some(b'6'..=b'9') => true,
        Some(b'5') => {
            frac.as_bytes()
                .get(10..)
                .is_some_and(|x| x.iter().any(|b| *b != b'0'))
                || n % 2 == 1
        }
        _ => return None,
    };
    n.checked_add(u64::from(round))
        .filter(|v| *v <= i64::MAX as u64)
}
fn signed_decimal_nanousd(value: &str) -> Option<i64> {
    let (negative, magnitude) = value
        .strip_prefix('-')
        .map_or((false, value), |v| (true, v));
    let magnitude = decimal_nanousd(magnitude)?;
    if negative {
        i64::try_from(magnitude).ok()?.checked_neg()
    } else {
        i64::try_from(magnitude).ok()
    }
}
fn db(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}

/// Provider cost with no matching local estimate is explicit unattributed
/// spend. A local estimate with no matching provider cost stays pending
/// until the settlement horizon, then becomes an explicit mismatch.
/// Unexplained variance beyond tolerance is reported as a mismatch, never
/// distributed to force totals to agree.
fn classify_reconciliation(
    local: Option<i64>,
    authoritative: Option<i64>,
    tolerance_nanousd: i64,
    day_end: DateTime<Utc>,
    settlement_horizon_days: i64,
    now: DateTime<Utc>,
) -> (ReconciliationStatus, Option<i64>) {
    match (local, authoritative) {
        (None, Some(_)) => (ReconciliationStatus::UnattributedProviderSpend, None),
        (Some(_), None) => {
            if now < day_end + Duration::days(settlement_horizon_days) {
                (ReconciliationStatus::Pending, None)
            } else {
                (ReconciliationStatus::Mismatch, None)
            }
        }
        (Some(local), Some(authoritative)) => {
            let variance = authoritative.saturating_sub(local);
            if variance == 0 {
                (ReconciliationStatus::Reconciled, Some(0))
            } else if variance.abs() <= tolerance_nanousd {
                (ReconciliationStatus::ReconciledWithVariance, Some(variance))
            } else {
                (ReconciliationStatus::Mismatch, Some(variance))
            }
        }
        (None, None) => (ReconciliationStatus::Pending, None),
    }
}

/// Settled (committed) PRD-064 reservations attributable to `project_id`'s
/// bound repositories, within the day — read as reconciliation evidence
/// only. This never mutates reservation state and never feeds back into
/// live warrant enforcement.
fn reservation_evidence(
    conn: &Connection,
    project_id: &str,
    day_start: DateTime<Utc>,
    day_end: DateTime<Utc>,
) -> familiar_ai_core::Result<(i64, Option<i64>)> {
    conn.query_row(
        "SELECT count(*),sum(ri.observed_amount) FROM resource_reservation_items ri JOIN resource_reservations r ON r.reservation_id=ri.reservation_id WHERE ri.resource_type=?1 AND r.state='committed' AND r.resolved_at>=?2 AND r.resolved_at<?3 AND r.project_id IN (SELECT evidence_value FROM project_registry_bindings WHERE project_id=?4 AND evidence_kind='repository')",
        params![
            ResourceType::NanousdBudget.as_str(),
            day_start.to_rfc3339(),
            day_end.to_rfc3339(),
            project_id
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(db)
}

const RECONCILIATION_COLUMNS: &str = "row_id,run_id,billing_source,day_start,day_end,match_key,project_id,status,local_estimate_nanousd,authoritative_nanousd,variance_nanousd,tolerance_nanousd,provider_revision_ids,observation_ids,reservation_evidence_count,reservation_evidence_nanousd,created_at";

fn map_reconciliation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReconciliationRow> {
    let provider_revision_ids: String = row.get(12)?;
    let observation_ids: String = row.get(13)?;
    Ok(ReconciliationRow {
        row_id: row.get(0)?,
        run_id: row.get(1)?,
        billing_source: row.get(2)?,
        day_start: row.get(3)?,
        day_end: row.get(4)?,
        match_key: row.get(5)?,
        project_id: row.get(6)?,
        status: row.get(7)?,
        local_estimate_nanousd: row.get(8)?,
        authoritative_nanousd: row.get(9)?,
        variance_nanousd: row.get(10)?,
        tolerance_nanousd: row.get(11)?,
        provider_revision_ids: serde_json::from_str(&provider_revision_ids).unwrap_or_default(),
        observation_ids: serde_json::from_str(&observation_ids).unwrap_or_default(),
        reservation_evidence_count: row.get(14)?,
        reservation_evidence_nanousd: row.get(15)?,
        created_at: row.get(16)?,
    })
}

fn fetch_reconciliation_row(
    conn: &Connection,
    row_id: &str,
) -> familiar_ai_core::Result<ReconciliationRow> {
    conn.query_row(
        &format!("SELECT {RECONCILIATION_COLUMNS} FROM reconciliation_rows WHERE row_id=?1"),
        [row_id],
        map_reconciliation_row,
    )
    .map_err(db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionHistoryRepository, ExecutionStart};
    use std::collections::BTreeMap;

    #[test]
    fn observations_are_distinct_idempotent_exact_and_append_only() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let schema: String = db
            .conn()
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='execution_history'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(schema.contains("execution_id TEXT PRIMARY KEY"));
        assert!(schema.contains("started_at TEXT NOT NULL"));
        assert!(schema.contains("outcome IN"));
        assert!(!schema.contains("agent = 'codex'"));
        ExecutionHistoryRepository::new(db.conn())
            .insert_running(&ExecutionStart {
                execution_id: "execution".into(),
                started_at: "2026-08-30T00:00:00Z".into(),
                repository: "/repo".into(),
                worktree: "/repo".into(),
                git_commit: None,
                prd_path: "docs/prds/PRD-051.md".into(),
                unavailable_fields: BTreeMap::new(),
            })
            .unwrap();
        let repo = AccountingRepository::new(db.conn());
        repo.register_project(
            "prj_fixture00000001",
            "fixture",
            "repository",
            "/machine/git/common",
            "test",
        )
        .unwrap();
        let value = UsageObservation {
            execution_id: "execution",
            attempt_id: "attempt-1",
            stage: "implementation",
            session_id: Some("safe-session"),
            worker_identity: "anthropic/claude",
            adapter: "claude-code",
            cli_version: Some("claude 1"),
            model_identity: Some("claude"),
            service_tier: None,
            provider_request_id: None,
            uncached_input_tokens: Some(1),
            cache_read_tokens: Some(2),
            cache_write_tokens: Some(3),
            output_tokens: Some(4),
            reasoning_output_tokens: Some(5),
            unknown_reason: None,
            period_start: "2026-08-30T00:00:00Z",
            period_end: "2026-08-30T00:00:01Z",
            terminal_status: "timed_out",
            source_event_hash: "sha256:fixture",
            provider_cost_lexical: Some("0.0000000015"),
            project_resolution_evidence: Some("/machine/git/common"),
            output_register_id: "none",
            output_register_version: "none",
            input_compression_id: "none",
            input_compression_version: "none",
            compression_experiment: None,
            compression_lane: None,
        };
        let observation = repo.append_observation(&value).unwrap().unwrap();
        assert_eq!(
            repo.append_observation(&value).unwrap().as_deref(),
            Some(observation.as_str())
        );
        repo.append_vendor_estimate(&observation, "0.0000000015")
            .unwrap();
        repo.append_vendor_estimate(&observation, "0.0000000015")
            .unwrap();
        let summary = repo.usage().unwrap();
        assert_eq!(summary.uncached_input_tokens, 1);
        assert_eq!(summary.cache_read_tokens, 2);
        assert_eq!(summary.cache_write_tokens, 3);
        assert_eq!(summary.reasoning_output_tokens, 5);
        assert_eq!(summary.known_nanousd, 2);
        assert!(db
            .conn()
            .execute(
                "UPDATE usage_observations SET output_tokens=0 WHERE observation_id=?1",
                [&observation],
            )
            .is_err());
        assert_eq!(
            repo.project_id("/machine/git/common").unwrap(),
            repo.project_id("/machine/git/common").unwrap()
        );
    }

    #[test]
    fn unresolved_identity_is_explicit_and_series_is_dense_and_bucketed_by_covered_time() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        for (id, prd) in [("exec-a", "PRD-A"), ("exec-b", "PRD-B")] {
            ExecutionHistoryRepository::new(db.conn())
                .insert_running(&ExecutionStart {
                    execution_id: id.into(),
                    started_at: "2026-08-01T00:00:00Z".into(),
                    repository: "/moved".into(),
                    worktree: "/alternate".into(),
                    git_commit: None,
                    prd_path: prd.into(),
                    unavailable_fields: BTreeMap::new(),
                })
                .unwrap();
        }
        let repo = AccountingRepository::new(db.conn());
        assert!(
            matches!(repo.resolve_project(Some("unbound")).unwrap(),ProjectIdentityResolution::Degraded{reason,..} if reason=="durable-project-unbound")
        );
        repo.register_project("prj_project0000001", "A", "repository", "repo-a", "test")
            .unwrap();
        repo.register_project("prj_project0000002", "B", "repository", "repo-b", "test")
            .unwrap();
        for (n, (execution, evidence, provider, time, tokens, cost)) in [
            (
                "1",
                (
                    "exec-a",
                    "repo-a",
                    "claude-code",
                    "2026-08-01T23:00:00Z",
                    10,
                    "1.0",
                ),
            ),
            (
                "2",
                (
                    "exec-b",
                    "repo-b",
                    "codex",
                    "2026-08-02T12:00:00Z",
                    20,
                    "2.0",
                ),
            ),
        ] {
            let value = UsageObservation {
                execution_id: execution,
                attempt_id: n,
                stage: "implementation",
                session_id: None,
                worker_identity: provider,
                adapter: provider,
                cli_version: None,
                model_identity: Some("model"),
                service_tier: None,
                provider_request_id: None,
                uncached_input_tokens: Some(tokens),
                cache_read_tokens: Some(0),
                cache_write_tokens: None,
                output_tokens: Some(1),
                reasoning_output_tokens: Some(0),
                unknown_reason: None,
                period_start: time,
                period_end: "2026-08-03T00:00:00Z",
                terminal_status: "succeeded",
                source_event_hash: n,
                provider_cost_lexical: Some(cost),
                project_resolution_evidence: Some(evidence),
                output_register_id: "none",
                output_register_version: "none",
                input_compression_id: "none",
                input_compression_version: "none",
                compression_experiment: None,
                compression_lane: None,
            };
            let id = repo.append_observation(&value).unwrap().unwrap();
            repo.append_vendor_estimate(&id, cost).unwrap();
        }
        let points = repo
            .usage_series(&UsageSeriesRequest {
                start: parse_utc("2026-08-01T00:00:00Z".into()).unwrap(),
                end: parse_utc("2026-08-04T00:00:00Z".into()).unwrap(),
                bucket: UsageBucket::Day,
                group_by: vec!["project".into(), "provider".into()],
                filters: BTreeMap::new(),
                dense: true,
            })
            .unwrap();
        assert_eq!(points.len(), 6); // two stable groups across three chart buckets
        assert_eq!(
            points
                .iter()
                .filter(|p| !p.observation_ids.is_empty())
                .count(),
            2
        );
        assert_eq!(
            points
                .iter()
                .filter_map(|p| p.estimated_cost_nanousd)
                .sum::<i64>(),
            3_000_000_000
        );
        assert!(points.iter().any(
            |p| p.bucket_start == "2026-08-01T00:00:00+00:00" && !p.observation_ids.is_empty()
        ));
    }

    #[test]
    fn openai_revisions_are_exact_idempotent_and_superseding() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db.conn().execute("INSERT INTO openai_billing_sources(source_id,provider_name,organization_id,project_id,admin_auth_env,created_at) VALUES('src','openai','org_a','proj_1','OPENAI_ADMIN_KEY','now')", []).unwrap();
        let repo = AccountingRepository::new(db.conn());
        let mut fact = OpenAiCostFact {
            source_id: "src",
            organization_id: "org_a",
            project_id: Some("proj_1"),
            bucket_start: 1,
            bucket_end: 2,
            line_item: "completions",
            classification: "usage",
            raw_amount_lexical: "0.0000000015",
            currency: "usd",
            payload_hash: "hash-1",
            collected_at: "2026-08-30T00:00:00Z",
        };
        assert!(repo.append_openai_cost_revision(&fact).unwrap());
        assert!(!repo.append_openai_cost_revision(&fact).unwrap());
        fact.raw_amount_lexical = "0.0000000025";
        fact.payload_hash = "hash-2";
        fact.collected_at = "2026-08-30T01:00:00Z";
        assert!(repo.append_openai_cost_revision(&fact).unwrap());
        let rows: (i64, i64, i64) = db.conn().query_row("SELECT count(*), max(amount_nanousd), sum(supersedes_revision_id IS NOT NULL) FROM openai_cost_revisions", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(rows, (2, 2, 1));
    }

    fn local_dollar_estimate(
        repo: &AccountingRepository<'_>,
        execution_id: &str,
        repository_evidence: &str,
        period_start: &str,
        lexical_usd: &str,
        hash: &str,
    ) {
        ExecutionHistoryRepository::new(repo.conn)
            .insert_running(&ExecutionStart {
                execution_id: execution_id.into(),
                started_at: period_start.into(),
                repository: repository_evidence.into(),
                worktree: repository_evidence.into(),
                git_commit: None,
                prd_path: "docs/prds/PRD-053.md".into(),
                unavailable_fields: BTreeMap::new(),
            })
            .unwrap();
        let value = UsageObservation {
            execution_id,
            attempt_id: "attempt-1",
            stage: "implementation",
            session_id: None,
            worker_identity: "anthropic/claude",
            adapter: "claude-code",
            cli_version: None,
            model_identity: Some("claude"),
            service_tier: None,
            provider_request_id: None,
            uncached_input_tokens: Some(1),
            cache_read_tokens: None,
            cache_write_tokens: None,
            output_tokens: Some(1),
            reasoning_output_tokens: None,
            unknown_reason: None,
            period_start,
            period_end: period_start,
            terminal_status: "succeeded",
            source_event_hash: hash,
            provider_cost_lexical: Some(lexical_usd),
            project_resolution_evidence: Some(repository_evidence),
            output_register_id: "none",
            output_register_version: "none",
            input_compression_id: "none",
            input_compression_version: "none",
            compression_experiment: None,
            compression_lane: None,
        };
        let observation = repo.append_observation(&value).unwrap().unwrap();
        repo.append_vendor_estimate(&observation, lexical_usd)
            .unwrap();
    }

    fn provider_row(workspace: &str, amount: &str) -> crate::repos::billing::ProviderCostRow {
        crate::repos::billing::ProviderCostRow {
            bucket_start: "2026-08-01T00:00:00Z".into(),
            bucket_end: "2026-08-02T00:00:00Z".into(),
            workspace_id: workspace.into(),
            description: "usage".into(),
            charge_class: "token-spend".into(),
            currency: "USD".into(),
            amount_lexical: amount.into(),
            provider_payload: format!("{{\"workspace\":\"{workspace}\",\"amount\":\"{amount}\"}}"),
        }
    }

    #[test]
    fn reconciliation_matches_variance_unattributed_and_pending_then_mismatch() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repo = AccountingRepository::new(db.conn());
        repo.register_project("prj_a00000000000001", "A", "repository", "repo-a", "test")
            .unwrap();
        repo.register_project("prj_b00000000000001", "B", "repository", "repo-b", "test")
            .unwrap();
        repo.register_project("prj_c00000000000001", "C", "repository", "repo-c", "test")
            .unwrap();
        local_dollar_estimate(
            &repo,
            "exec-a",
            "repo-a",
            "2026-08-01T10:00:00Z",
            "1.00",
            "h-a",
        );
        local_dollar_estimate(
            &repo,
            "exec-b",
            "repo-b",
            "2026-08-01T10:00:00Z",
            "1.00",
            "h-b",
        );
        local_dollar_estimate(
            &repo,
            "exec-c",
            "repo-c",
            "2026-08-01T10:00:00Z",
            "0.50",
            "h-c",
        );
        repo.bind_provider(
            "prj_a00000000000001",
            "org-main",
            "workspace",
            "wrk_a",
            "exact",
            "test",
        )
        .unwrap();
        repo.bind_provider(
            "prj_b00000000000001",
            "org-main",
            "workspace",
            "wrk_b",
            "exact",
            "test",
        )
        .unwrap();

        let billing = crate::repos::billing::BillingRepository::new(db.conn());
        billing
            .bind_source(&crate::repos::billing::BillingSource {
                name: "org-main",
                mode: "anthropic-organization",
                organization_id: "org_main",
                organization_name: "Main",
                credential_reference: "env: ADMIN_MAIN",
            })
            .unwrap();
        billing
            .commit_complete(
                "org-main",
                "2026-08-01T00:00:00Z",
                "2026-08-02T00:00:00Z",
                &[
                    provider_row("wrk_a", "1.00"),
                    provider_row("wrk_b", "1.01"),
                    provider_row("wrk_unbound", "2.00"),
                ],
            )
            .unwrap();

        let start = parse_utc("2026-08-01T00:00:00Z".into()).unwrap();
        let end = parse_utc("2026-08-02T00:00:00Z".into()).unwrap();
        let within_horizon = parse_utc("2026-08-01T12:00:00Z".into()).unwrap();
        let summary = repo
            .reconcile_window(
                "org-main",
                start,
                end,
                "explicit",
                10_000_000,
                3,
                within_horizon,
                "operator",
            )
            .unwrap();
        assert_eq!(summary.rows_appended, 4);
        assert_eq!(summary.rows_unchanged, 0);
        let by_key = |rows: &[ReconciliationRow], key: &str| {
            rows.iter().find(|r| r.match_key == key).cloned().unwrap()
        };
        let a = by_key(&summary.rows, "project:prj_a00000000000001");
        assert_eq!(a.status, "reconciled");
        assert_eq!(a.variance_nanousd, Some(0));
        assert_eq!(a.local_estimate_nanousd, Some(1_000_000_000));
        assert_eq!(a.authoritative_nanousd, Some(1_000_000_000));

        let b = by_key(&summary.rows, "project:prj_b00000000000001");
        assert_eq!(b.status, "reconciled-with-variance");
        assert_eq!(b.variance_nanousd, Some(10_000_000));

        let unattributed = by_key(&summary.rows, "unattributed");
        assert_eq!(unattributed.status, "unattributed-provider-spend");
        assert_eq!(unattributed.project_id, None);
        assert_eq!(unattributed.authoritative_nanousd, Some(2_000_000_000));
        assert_eq!(unattributed.local_estimate_nanousd, None);

        let c = by_key(&summary.rows, "project:prj_c00000000000001");
        assert_eq!(c.status, "pending");
        assert_eq!(c.local_estimate_nanousd, Some(500_000_000));
        assert_eq!(c.authoritative_nanousd, None);

        // Re-running the identical window is idempotent: no new rows.
        let rerun = repo
            .reconcile_window(
                "org-main",
                start,
                end,
                "explicit",
                10_000_000,
                3,
                within_horizon,
                "operator",
            )
            .unwrap();
        assert_eq!(rerun.rows_appended, 0);
        assert_eq!(rerun.rows_unchanged, 4);

        // Past the settlement horizon, project C's unmatched local estimate
        // becomes an explicit mismatch instead of staying pending forever.
        let past_horizon = parse_utc("2026-08-06T00:00:00Z".into()).unwrap();
        let later = repo
            .reconcile_window(
                "org-main",
                start,
                end,
                "explicit",
                10_000_000,
                3,
                past_horizon,
                "operator",
            )
            .unwrap();
        assert_eq!(later.rows_appended, 1);
        assert_eq!(later.rows_unchanged, 3);
        let c2 = by_key(&later.rows, "project:prj_c00000000000001");
        assert_eq!(c2.status, "mismatch");

        // history is preserved: two rows now exist for project C.
        let total: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM reconciliation_rows WHERE match_key='project:prj_c00000000000001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 2);
    }

    #[test]
    fn reconciliation_reopens_window_on_superseding_provider_revision() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repo = AccountingRepository::new(db.conn());
        repo.register_project("prj_a00000000000002", "A", "repository", "repo-a", "test")
            .unwrap();
        local_dollar_estimate(
            &repo,
            "exec-a",
            "repo-a",
            "2026-08-01T10:00:00Z",
            "1.00",
            "h-a",
        );
        repo.bind_provider(
            "prj_a00000000000002",
            "org-main",
            "workspace",
            "wrk_a",
            "exact",
            "test",
        )
        .unwrap();
        let billing = crate::repos::billing::BillingRepository::new(db.conn());
        billing
            .bind_source(&crate::repos::billing::BillingSource {
                name: "org-main",
                mode: "anthropic-organization",
                organization_id: "org_main",
                organization_name: "Main",
                credential_reference: "env: ADMIN_MAIN",
            })
            .unwrap();
        billing
            .commit_complete(
                "org-main",
                "2026-08-01T00:00:00Z",
                "2026-08-02T00:00:00Z",
                &[provider_row("wrk_a", "1.00")],
            )
            .unwrap();
        let start = parse_utc("2026-08-01T00:00:00Z".into()).unwrap();
        let end = parse_utc("2026-08-02T00:00:00Z".into()).unwrap();
        let now = parse_utc("2026-08-01T12:00:00Z".into()).unwrap();
        let first = repo
            .reconcile_window(
                "org-main", start, end, "collect", 10_000_000, 3, now, "system",
            )
            .unwrap();
        assert_eq!(first.rows_appended, 1);
        assert_eq!(first.rows[0].status, "reconciled");
        let first_row_id = first.rows[0].row_id.clone();

        // A provider correction supersedes the original revision.
        billing
            .commit_complete(
                "org-main",
                "2026-08-01T00:00:00Z",
                "2026-08-02T00:00:00Z",
                &[provider_row("wrk_a", "1.50")],
            )
            .unwrap();
        let second = repo
            .reconcile_window(
                "org-main", start, end, "collect", 10_000_000, 3, now, "system",
            )
            .unwrap();
        assert_eq!(second.rows_appended, 1);
        assert_eq!(second.rows[0].status, "mismatch");
        assert_eq!(second.rows[0].authoritative_nanousd, Some(1_500_000_000));
        let row_id: String = db
            .conn()
            .query_row(
                "SELECT supersedes_row_id FROM reconciliation_rows WHERE row_id=?1",
                [&second.rows[0].row_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_id, first_row_id);

        // History is retained: both rows still exist; only the newer is
        // current-effective.
        let total: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM reconciliation_rows", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2);
        let current: Vec<String> = {
            let mut statement = db
                .conn()
                .prepare("SELECT row_id FROM current_reconciliation")
                .unwrap();
            statement
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(current, vec![second.rows[0].row_id.clone()]);
    }

    #[test]
    fn month_to_date_and_prd_cost_score_labels_authority_and_unknown() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repo = AccountingRepository::new(db.conn());
        repo.register_project("prj_m00000000000001", "M", "repository", "repo-m", "test")
            .unwrap();
        local_dollar_estimate(
            &repo,
            "exec-m",
            "repo-m",
            "2026-08-01T10:00:00Z",
            "1.00",
            "h-m",
        );
        repo.bind_provider(
            "prj_m00000000000001",
            "org-main",
            "workspace",
            "wrk_m",
            "exact",
            "test",
        )
        .unwrap();
        let billing = crate::repos::billing::BillingRepository::new(db.conn());
        billing
            .bind_source(&crate::repos::billing::BillingSource {
                name: "org-main",
                mode: "anthropic-organization",
                organization_id: "org_main",
                organization_name: "Main",
                credential_reference: "env: ADMIN_MAIN",
            })
            .unwrap();
        billing
            .commit_complete(
                "org-main",
                "2026-08-01T00:00:00Z",
                "2026-08-02T00:00:00Z",
                &[provider_row("wrk_m", "1.00")],
            )
            .unwrap();
        let start = parse_utc("2026-08-01T00:00:00Z".into()).unwrap();
        let end = parse_utc("2026-08-02T00:00:00Z".into()).unwrap();
        let now = parse_utc("2026-08-15T00:00:00Z".into()).unwrap();
        repo.reconcile_window(
            "org-main", start, end, "explicit", 10_000_000, 3, now, "test",
        )
        .unwrap();

        let months = repo.month_to_date_by_source(now).unwrap();
        assert_eq!(months.len(), 1);
        assert_eq!(months[0].billing_source, "org-main");
        assert_eq!(months[0].reconciled_days, 1);
        assert_eq!(months[0].authoritative_nanousd, Some(1_000_000_000));
        assert_eq!(months[0].local_estimate_nanousd, Some(1_000_000_000));
        assert_eq!(months[0].completeness, "complete");

        // The aggregate travels alongside the per-source breakdown, never
        // instead of it.
        let report = repo.month_to_date_report(now).unwrap();
        assert_eq!(report.sources, months);
        assert_eq!(report.aggregate.source_count, 1);
        assert_eq!(report.aggregate.authoritative_nanousd, Some(1_000_000_000));
        assert_eq!(report.aggregate.local_estimate_nanousd, Some(1_000_000_000));

        // A separate execution with no cost estimate at all is unknown, not
        // free — it must never be exposed as a rankable-cheap zero cost.
        ExecutionHistoryRepository::new(db.conn())
            .insert_running(&ExecutionStart {
                execution_id: "exec-unknown".into(),
                started_at: "2026-08-01T10:00:00Z".into(),
                repository: "repo-m".into(),
                worktree: "repo-m".into(),
                git_commit: None,
                prd_path: "docs/prds/PRD-053.md".into(),
                unavailable_fields: BTreeMap::new(),
            })
            .unwrap();
        let unknown = AccountingRepository::new(db.conn());
        unknown
            .append_observation(&UsageObservation {
                execution_id: "exec-unknown",
                attempt_id: "attempt-1",
                stage: "implementation",
                session_id: None,
                worker_identity: "codex/gpt",
                adapter: "codex",
                cli_version: None,
                model_identity: None,
                service_tier: None,
                provider_request_id: None,
                uncached_input_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                output_tokens: None,
                reasoning_output_tokens: None,
                unknown_reason: Some("adapter never reports usage"),
                period_start: "2026-08-01T10:00:00Z",
                period_end: "2026-08-01T10:00:00Z",
                terminal_status: "succeeded",
                source_event_hash: "h-unknown",
                provider_cost_lexical: None,
                project_resolution_evidence: Some("repo-m"),
                output_register_id: "none",
                output_register_version: "none",
                input_compression_id: "none",
                input_compression_version: "none",
                compression_experiment: None,
                compression_lane: None,
            })
            .unwrap();

        let scores = repo.accepted_prd_cost().unwrap();
        let known = scores
            .iter()
            .find(|s| s.worker_identity == "anthropic/claude")
            .unwrap();
        assert_eq!(known.authority, "estimated");
        assert_eq!(known.local_estimate_nanousd, Some(1_000_000_000));
        let unknown_score = scores
            .iter()
            .find(|s| s.worker_identity == "codex/gpt")
            .unwrap();
        assert_eq!(unknown_score.authority, "unknown");
        assert_eq!(unknown_score.local_estimate_nanousd, None);
    }
}
