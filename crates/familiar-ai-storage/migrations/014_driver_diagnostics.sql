ALTER TABLE driver_sessions ADD COLUMN worker_id TEXT;
ALTER TABLE driver_sessions ADD COLUMN heartbeat_at TEXT;

ALTER TABLE driver_attempts ADD COLUMN adapter_id TEXT;
ALTER TABLE driver_attempts ADD COLUMN model TEXT;
ALTER TABLE driver_attempts ADD COLUMN exit_code INTEGER;
ALTER TABLE driver_attempts ADD COLUMN signal INTEGER;
ALTER TABLE driver_attempts ADD COLUMN last_durable_phase TEXT;
