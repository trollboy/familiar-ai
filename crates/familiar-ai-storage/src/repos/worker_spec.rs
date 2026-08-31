use chrono::Utc;
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection};

pub struct WorkerSpecRepository<'a> {
    conn: &'a Connection,
}

impl<'a> WorkerSpecRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_spec(
        &self,
        spec: &str,
        version: &str,
        alias: &str,
        provider: &str,
        runtime: &str,
        model: &str,
        artifact: Option<&str>,
        auth_profile: Option<&str>,
        capability_profile: &str,
        material_parameters_json: &str,
    ) -> familiar_ai_core::Result<()> {
        let (model_state, model_id) = match model {
            "unknown" => ("unknown", None),
            "runtime-selected" => ("runtime-selected", None),
            known => ("known", Some(known)),
        };
        let now = Utc::now().to_rfc3339();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|error| FamiliarError::Database(error.to_string()))?;
        tx.execute("INSERT OR IGNORE INTO worker_specs(spec_identity,worker_alias,provider_id,runtime_id,model_state,model_id,model_artifact_id,auth_profile_id,capability_profile_id,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![spec,alias,provider,runtime,model_state,model_id,artifact,auth_profile,capability_profile,now]).map_err(|error| FamiliarError::Database(error.to_string()))?;
        tx.execute("INSERT OR IGNORE INTO worker_spec_versions(empirical_version,spec_identity,material_parameters_json,adapter_schema_version,created_at) VALUES(?1,?2,?3,'prd-057-v1',?4)", params![version,spec,material_parameters_json,now]).map_err(|error| FamiliarError::Database(error.to_string()))?;
        tx.commit()
            .map_err(|error| FamiliarError::Database(error.to_string()))?;
        Ok(())
    }

    /// Probe failures must be supplied as `unknown`; storage never promotes a
    /// failed/unavailable check or infers a fact from a provider name.
    pub fn record_capability(
        &self,
        spec: &str,
        capability: &str,
        provenance: &str,
    ) -> familiar_ai_core::Result<()> {
        self.conn.execute(
            "INSERT INTO worker_capabilities(spec_identity,capability,provenance,recorded_at) VALUES(?1,?2,?3,?4) ON CONFLICT(spec_identity,capability) DO UPDATE SET provenance=excluded.provenance,recorded_at=excluded.recorded_at",
            params![spec, capability, provenance, Utc::now().to_rfc3339()],
        ).map_err(|error| FamiliarError::Database(format!("worker capability write failed: {error}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_provenance_transitions_are_explicit_and_closed() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db.conn().execute("INSERT INTO worker_specs(spec_identity,worker_alias,provider_id,runtime_id,model_state,model_id,capability_profile_id,created_at) VALUES('spec','worker','openai','codex','runtime-selected',NULL,'profile','now')", []).unwrap();
        let repo = WorkerSpecRepository::new(db.conn());
        repo.record_capability("spec", "streaming", "unknown")
            .unwrap();
        repo.record_capability("spec", "streaming", "observed")
            .unwrap();
        let provenance: String = db.conn().query_row("SELECT provenance FROM worker_capabilities WHERE spec_identity='spec' AND capability='streaming'", [], |row| row.get(0)).unwrap();
        assert_eq!(provenance, "observed");
        assert!(repo
            .record_capability("spec", "imagined-from-provider", "declared")
            .is_err());
        assert!(repo
            .record_capability("spec", "streaming", "assumed")
            .is_err());
    }
}
