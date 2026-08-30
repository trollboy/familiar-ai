ALTER TABLE delivery_external_effects ADD COLUMN target TEXT NULL;
ALTER TABLE delivery_external_effects ADD COLUMN revision TEXT NULL;
ALTER TABLE delivery_external_effects ADD COLUMN output BLOB NULL;

CREATE INDEX delivery_effect_evidence_idx
ON delivery_external_effects(repository_key, prd_id, effect_kind, target, revision, updated_at);
