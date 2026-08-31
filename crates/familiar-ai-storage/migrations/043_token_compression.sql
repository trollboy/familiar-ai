-- PRD-069 compression attribution. No prompt, response, source, or other
-- provider-bound content is stored here.
ALTER TABLE usage_observations ADD COLUMN output_register_id TEXT NOT NULL DEFAULT 'none';
ALTER TABLE usage_observations ADD COLUMN output_register_version TEXT NOT NULL DEFAULT 'none';
ALTER TABLE usage_observations ADD COLUMN input_compression_id TEXT NOT NULL DEFAULT 'none';
ALTER TABLE usage_observations ADD COLUMN input_compression_version TEXT NOT NULL DEFAULT 'none';
ALTER TABLE usage_observations ADD COLUMN compression_experiment TEXT;
ALTER TABLE usage_observations ADD COLUMN compression_lane TEXT;

CREATE INDEX idx_usage_observations_compression
ON usage_observations(
    output_register_id, output_register_version,
    input_compression_id, input_compression_version,
    compression_experiment, compression_lane
);

