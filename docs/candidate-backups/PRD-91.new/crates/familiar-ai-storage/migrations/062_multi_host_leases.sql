-- PRD-091: durable, host-identified, expiring driver leases. One row per
-- repository; the filesystem control-plane claim keeps doing local exclusion
-- unchanged, this table is the cross-host authority. Expiry and "remaining
-- life" are always computed from store-side time (julianday('now')), never
-- host wall-clock, per the recorded clock-skew assumption.
CREATE TABLE control_plane_leases (
    repository_key TEXT PRIMARY KEY,
    host_identity TEXT NOT NULL,
    process_identity TEXT NOT NULL,
    holder_token TEXT NOT NULL,
    acquired_at TEXT NOT NULL,
    renewed_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    lease_generation INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE control_plane_lease_events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_key TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('acquired','takeover','renewed','released','lost')),
    prior_host_identity TEXT,
    prior_process_identity TEXT,
    new_host_identity TEXT,
    new_process_identity TEXT,
    detail TEXT,
    recorded_at TEXT NOT NULL
);
CREATE INDEX control_plane_lease_events_by_repo ON control_plane_lease_events(repository_key, event_id);
