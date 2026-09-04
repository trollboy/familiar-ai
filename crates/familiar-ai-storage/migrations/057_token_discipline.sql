-- PRD-072 raw-runtime token-discipline attribution: identity/version pairs
-- for the edit-form and truncation-window policy active on a run, mirroring
-- migration 043's compression attribution. No prompt, response, source, or
-- tool output is stored here — only policy identity.
ALTER TABLE usage_observations ADD COLUMN edit_form_id TEXT NOT NULL DEFAULT 'none';
ALTER TABLE usage_observations ADD COLUMN edit_form_version TEXT NOT NULL DEFAULT 'none';
ALTER TABLE usage_observations ADD COLUMN truncation_config_id TEXT NOT NULL DEFAULT 'none';
ALTER TABLE usage_observations ADD COLUMN truncation_config_version TEXT NOT NULL DEFAULT 'none';

CREATE INDEX idx_usage_observations_token_discipline
ON usage_observations(
    edit_form_id, edit_form_version,
    truncation_config_id, truncation_config_version
);
