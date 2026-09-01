use familiar_ai_core::config::{ArtifactVerificationState, ModelArtifactConfig};
use familiar_ai_core::FamiliarError;
use rusqlite::{params, OptionalExtension};

use super::now_rfc3339;
use crate::Database;

pub struct ModelArtifactRepository<'a> {
    db: &'a Database,
}

impl<'a> ModelArtifactRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Insert immutable content and bind its friendly alias. Identical input
    /// is a no-op; either an id collision or alias rewrite fails closed.
    pub fn register(
        &self,
        alias: &str,
        artifact: &ModelArtifactConfig,
    ) -> familiar_ai_core::Result<bool> {
        let manifest = artifact
            .manifest
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| FamiliarError::Database(error.to_string()))?;
        let provenance = serde_json::to_string(&artifact.provenance)
            .map_err(|error| FamiliarError::Database(error.to_string()))?;
        let state = match artifact.state {
            ArtifactVerificationState::Verified => "verified",
            ArtifactVerificationState::DegradedUnverifiedAlias => "degraded-unverified-alias",
        };
        let existing: Option<(String, Option<String>, String)> = self.db.conn().query_row(
            "SELECT verification_state,manifest_json,provenance_json FROM model_artifacts WHERE model_artifact_id=?1",
            [&artifact.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional().map_err(db)?;
        if let Some(existing) = existing {
            if existing != (state.into(), manifest.clone(), provenance.clone()) {
                return Err(FamiliarError::Database(
                    "immutable artifact entry already exists with different content".into(),
                ));
            }
        }
        let bound: Option<String> = self
            .db
            .conn()
            .query_row(
                "SELECT model_artifact_id FROM model_artifact_aliases WHERE alias=?1",
                [alias],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?;
        if let Some(bound) = bound {
            if bound == artifact.id {
                return Ok(false);
            }
            return Err(FamiliarError::Database(format!(
                "artifact alias '{alias}' is already bound and cannot be rewritten"
            )));
        }
        let tx = self.db.conn().unchecked_transaction().map_err(db)?;
        tx.execute("INSERT OR IGNORE INTO model_artifacts(model_artifact_id,verification_state,manifest_json,provenance_json,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![artifact.id,state,manifest,provenance,now_rfc3339()]).map_err(db)?;
        tx.execute("INSERT INTO model_artifact_aliases(alias,model_artifact_id,created_at) VALUES(?1,?2,?3)",
            params![alias,artifact.id,now_rfc3339()]).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(true)
    }

    pub fn get(&self, id: &str) -> familiar_ai_core::Result<Option<ModelArtifactConfig>> {
        self.db.conn().query_row(
            "SELECT verification_state,manifest_json,provenance_json FROM model_artifacts WHERE model_artifact_id=?1",
            [id], |row| {
                let state: String = row.get(0)?;
                let manifest: Option<String> = row.get(1)?;
                let provenance: String = row.get(2)?;
                Ok((state, manifest, provenance))
            },
        ).optional().map_err(db)?.map(|(state, manifest, provenance)| Ok(ModelArtifactConfig {
            id: id.into(),
            state: if state == "verified" { ArtifactVerificationState::Verified } else { ArtifactVerificationState::DegradedUnverifiedAlias },
            runtime_alias: None,
            manifest: manifest.map(|value| serde_json::from_str(&value)).transpose().map_err(json)?,
            provenance: serde_json::from_str(&provenance).map_err(json)?,
        })).transpose()
    }
}

fn db(error: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}
fn json(error: serde_json::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::config::{
        ArtifactManifest, ModelArtifactProvenance, ARTIFACT_DIGEST_ALGORITHM,
        ARTIFACT_MANIFEST_SCHEMA,
    };

    fn artifact(id: &str) -> ModelArtifactConfig {
        ModelArtifactConfig {
            id: id.into(),
            state: ArtifactVerificationState::Verified,
            runtime_alias: None,
            manifest: Some(ArtifactManifest {
                schema_version: ARTIFACT_MANIFEST_SCHEMA,
                digest_algorithm: ARTIFACT_DIGEST_ALGORITHM.into(),
                files: vec![],
                base_artifact: Some("base".into()),
                adapters: vec![],
                merged: false,
                identity: Default::default(),
            }),
            provenance: ModelArtifactProvenance::default(),
        }
    }

    #[test]
    fn registration_is_idempotent_and_aliases_never_rewrite() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repo = ModelArtifactRepository::new(&db);
        let first = artifact(&format!("sha256:{}", "a".repeat(64)));
        let second = artifact(&format!("sha256:{}", "b".repeat(64)));
        assert!(repo.register("friendly", &first).unwrap());
        assert!(!repo.register("friendly", &first).unwrap());
        assert!(repo.register("friendly", &second).is_err());
    }

    #[test]
    fn same_friendly_lineage_can_have_separate_registry_entries() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repo = ModelArtifactRepository::new(&db);
        let a = artifact(&format!("sha256:{}", "a".repeat(64)));
        let b = artifact(&format!("sha256:{}", "b".repeat(64)));
        repo.register("model-v1", &a).unwrap();
        repo.register("model-v2", &b).unwrap();
        assert_ne!(
            repo.get(&a.id).unwrap().unwrap().id,
            repo.get(&b.id).unwrap().unwrap().id
        );
    }
}
