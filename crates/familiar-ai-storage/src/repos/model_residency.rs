//! PRD-073 warm local model residency records (migration 073).
//!
//! Every transition the daemon's residency manager makes — a model loaded,
//! a resident evicted at a ceiling, a health probe that failed, a bounded
//! restart, a final failure, a stop — lands here as an append-only fact.
//! The point of the table is the fourth acceptance criterion: a resident
//! that dies must never quietly degrade into per-call loading. If the
//! degradation is not in this table, it did not happen.

use chrono::Utc;
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{params, Connection};

use familiar_ai_core::FamiliarError;

fn db(error: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}

fn random_hex() -> familiar_ai_core::Result<String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| FamiliarError::Database("secure residency id generation failed".into()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The closed residency lifecycle vocabulary. `Failed` is terminal for one
/// resident: restarts are exhausted and the daemon has stopped trying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyEvent {
    Loaded,
    Stopped,
    Evicted,
    Restarted,
    Failed,
    HealthFailed,
}

impl ResidencyEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::Stopped => "stopped",
            Self::Evicted => "evicted",
            Self::Restarted => "restarted",
            Self::Failed => "failed",
            Self::HealthFailed => "health-failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "loaded" => Self::Loaded,
            "stopped" => Self::Stopped,
            "evicted" => Self::Evicted,
            "restarted" => Self::Restarted,
            "failed" => Self::Failed,
            "health-failed" => Self::HealthFailed,
            _ => return None,
        })
    }

    /// Whether this event requires a named reason. Enforced in SQL too; the
    /// Rust check exists so the caller gets a useful error rather than a
    /// constraint violation.
    const fn requires_reason(self) -> bool {
        matches!(
            self,
            Self::Stopped | Self::Evicted | Self::Restarted | Self::Failed | Self::HealthFailed
        )
    }
}

#[derive(Debug, Clone)]
pub struct ResidencyEventRow<'a> {
    pub resident_key: &'a str,
    pub worker_identity: &'a str,
    pub model_artifact_id: &'a str,
    pub runtime_id: &'a str,
    pub server_identity: &'a str,
    pub event: ResidencyEvent,
    pub reason: Option<&'a str>,
    pub restart_attempt: Option<u32>,
    pub memory_mb: Option<u64>,
}

/// A recorded event read back, with its own id and timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedResidencyEvent {
    pub event_id: String,
    pub resident_key: String,
    pub worker_identity: String,
    pub model_artifact_id: String,
    pub runtime_id: String,
    pub server_identity: String,
    pub event: ResidencyEvent,
    pub reason: Option<String>,
    pub restart_attempt: Option<u32>,
    pub memory_mb: Option<u64>,
    pub recorded_at: String,
}

/// How one execution's serving process was reached, recorded against a
/// PRD-051 observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyStateRecord {
    Cold,
    Warm,
}

impl ResidencyStateRecord {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warm => "warm",
        }
    }
}

/// What the serving runtime actually reported about its prefix cache.
/// `Unknown` carries the reason it is unknown; it is never inferred from
/// residency state, and residency state is never inferred from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheEvidenceRecord {
    ColdLoad,
    WarmMiss,
    WarmHit,
    Unknown,
}

impl CacheEvidenceRecord {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ColdLoad => "cold-load",
            Self::WarmMiss => "warm-miss",
            Self::WarmHit => "warm-hit",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ObservationResidency<'a> {
    pub residency_state: ResidencyStateRecord,
    pub resident_server_identity: Option<&'a str>,
    pub cache_evidence: CacheEvidenceRecord,
    pub cache_evidence_reason: Option<&'a str>,
}

/// One residency partition of local telemetry: how many runs served at
/// this residency state, and their mean latency, load time and accelerator
/// utilization. A mean is `None` when no run in the partition reported the
/// underlying measurement — absent, never a fabricated zero.
#[derive(Debug, Clone, PartialEq)]
pub struct ResidencyPartition {
    pub residency_state: String,
    pub runs: u64,
    pub mean_wall_time_ms: Option<f64>,
    pub mean_load_time_ms: Option<f64>,
    pub mean_accelerator_utilization_pct: Option<f64>,
}

/// Residency attribution read back for one observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedObservationResidency {
    pub residency_state: String,
    pub resident_server_identity: Option<String>,
    pub cache_evidence: String,
    pub cache_evidence_reason: Option<String>,
}

pub struct ModelResidencyRepository<'a> {
    conn: &'a Connection,
}

impl<'a> ModelResidencyRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn record(&self, row: &ResidencyEventRow<'_>) -> familiar_ai_core::Result<String> {
        if row.event.requires_reason()
            && !row.reason.is_some_and(|reason| !reason.trim().is_empty())
        {
            return Err(FamiliarError::Database(format!(
                "residency event '{}' requires a named reason",
                row.event.as_str()
            )));
        }
        if (row.event == ResidencyEvent::Restarted) != row.restart_attempt.is_some() {
            return Err(FamiliarError::Database(
                "restart_attempt belongs to a 'restarted' event and to no other".into(),
            ));
        }
        let event_id = format!("res_{}", random_hex()?);
        self.conn
            .execute(
                "INSERT INTO model_residency_events(event_id,resident_key,worker_identity,model_artifact_id,runtime_id,server_identity,event,reason,restart_attempt,memory_mb,recorded_at) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    event_id,
                    row.resident_key,
                    row.worker_identity,
                    row.model_artifact_id,
                    row.runtime_id,
                    row.server_identity,
                    row.event.as_str(),
                    row.reason,
                    row.restart_attempt,
                    row.memory_mb,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(db)?;
        Ok(event_id)
    }

    /// Every event for one resident key, oldest first.
    pub fn events_for(
        &self,
        resident_key: &str,
    ) -> familiar_ai_core::Result<Vec<RecordedResidencyEvent>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT event_id,resident_key,worker_identity,model_artifact_id,runtime_id,server_identity,event,reason,restart_attempt,memory_mb,recorded_at \
                 FROM model_residency_events WHERE resident_key=?1 ORDER BY recorded_at, rowid",
            )
            .map_err(db)?;
        let rows = statement
            .query_map([resident_key], |row| {
                let event: String = row.get(6)?;
                Ok(RecordedResidencyEvent {
                    event_id: row.get(0)?,
                    resident_key: row.get(1)?,
                    worker_identity: row.get(2)?,
                    model_artifact_id: row.get(3)?,
                    runtime_id: row.get(4)?,
                    server_identity: row.get(5)?,
                    event: ResidencyEvent::parse(&event).unwrap_or(ResidencyEvent::Failed),
                    reason: row.get(7)?,
                    restart_attempt: row.get(8)?,
                    memory_mb: row.get(9)?,
                    recorded_at: row.get(10)?,
                })
            })
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    /// How many times this resident's model has actually been loaded — the
    /// measurement behind "two consecutive calls incur one model load".
    pub fn load_count(&self, resident_key: &str) -> familiar_ai_core::Result<u64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM model_residency_events WHERE resident_key=?1 AND event='loaded'",
                [resident_key],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as u64)
            .map_err(db)
    }

    pub fn recent(&self, limit: usize) -> familiar_ai_core::Result<Vec<RecordedResidencyEvent>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT event_id,resident_key,worker_identity,model_artifact_id,runtime_id,server_identity,event,reason,restart_attempt,memory_mb,recorded_at \
                 FROM model_residency_events ORDER BY recorded_at DESC, rowid DESC LIMIT ?1",
            )
            .map_err(db)?;
        let rows = statement
            .query_map([limit as i64], |row| {
                let event: String = row.get(6)?;
                Ok(RecordedResidencyEvent {
                    event_id: row.get(0)?,
                    resident_key: row.get(1)?,
                    worker_identity: row.get(2)?,
                    model_artifact_id: row.get(3)?,
                    runtime_id: row.get(4)?,
                    server_identity: row.get(5)?,
                    event: ResidencyEvent::parse(&event).unwrap_or(ResidencyEvent::Failed),
                    reason: row.get(7)?,
                    restart_attempt: row.get(8)?,
                    memory_mb: row.get(9)?,
                    recorded_at: row.get(10)?,
                })
            })
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    /// Partitions PRD-063 local telemetry by residency state — the point of
    /// recording it. Rows that predate residency (`residency_state IS NULL`)
    /// are excluded rather than folded into `cold`: "ran before residency
    /// existed" and "ran cold" are different facts.
    pub fn telemetry_by_residency(
        &self,
        worker_identity: Option<&str>,
    ) -> familiar_ai_core::Result<Vec<ResidencyPartition>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT residency_state,COUNT(*),AVG(wall_time_ms),AVG(load_time_ms),AVG(accelerator_utilization_pct) \
                 FROM local_worker_telemetry \
                 WHERE residency_state IS NOT NULL AND (?1 IS NULL OR worker_identity=?1) \
                 GROUP BY residency_state ORDER BY residency_state",
            )
            .map_err(db)?;
        let rows = statement
            .query_map([worker_identity], |row| {
                Ok(ResidencyPartition {
                    residency_state: row.get(0)?,
                    runs: row.get::<_, i64>(1)? as u64,
                    mean_wall_time_ms: row.get(2)?,
                    mean_load_time_ms: row.get(3)?,
                    mean_accelerator_utilization_pct: row.get(4)?,
                })
            })
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    /// Attaches residency attribution to one PRD-051 observation.
    pub fn attach_to_observation(
        &self,
        observation_id: &str,
        residency: &ObservationResidency<'_>,
    ) -> familiar_ai_core::Result<()> {
        if residency.residency_state == ResidencyStateRecord::Warm
            && residency.resident_server_identity.is_none()
        {
            return Err(FamiliarError::Database(
                "a warm observation must name the resident server that served it".into(),
            ));
        }
        if residency.cache_evidence == CacheEvidenceRecord::Unknown
            && !residency
                .cache_evidence_reason
                .is_some_and(|reason| !reason.trim().is_empty())
        {
            return Err(FamiliarError::Database(
                "unknown cache evidence must record why it is unknown".into(),
            ));
        }
        self.conn
            .execute(
                "INSERT INTO usage_observation_residency(observation_id,residency_state,resident_server_identity,cache_evidence,cache_evidence_reason,recorded_at) \
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    observation_id,
                    residency.residency_state.as_str(),
                    residency.resident_server_identity,
                    residency.cache_evidence.as_str(),
                    residency.cache_evidence_reason,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(db)?;
        Ok(())
    }

    /// Residency attribution for one observation, absent when the execution
    /// recorded none.
    pub fn observation_residency(
        &self,
        observation_id: &str,
    ) -> familiar_ai_core::Result<Option<RecordedObservationResidency>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT residency_state,resident_server_identity,cache_evidence,cache_evidence_reason \
                 FROM usage_observation_residency WHERE observation_id=?1",
                [observation_id],
                |row| {
                    Ok(RecordedObservationResidency {
                        residency_state: row.get(0)?,
                        resident_server_identity: row.get(1)?,
                        cache_evidence: row.get(2)?,
                        cache_evidence_reason: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(db)
    }
}
