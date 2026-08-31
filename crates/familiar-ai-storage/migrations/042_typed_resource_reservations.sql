CREATE TABLE resource_pools (
    pool_id TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    capacity INTEGER NOT NULL CHECK(capacity >= 0),
    available INTEGER NOT NULL CHECK(available >= 0),
    renewable INTEGER NOT NULL DEFAULT 0 CHECK(renewable IN (0,1)),
    PRIMARY KEY(pool_id, resource_type),
    CHECK(available <= capacity)
);

CREATE TABLE resource_reservations (
    reservation_id TEXT PRIMARY KEY,
    owner_instance_id TEXT NOT NULL UNIQUE,
    installation_id TEXT,
    nonce_or_generation TEXT NOT NULL,
    owner_kind TEXT NOT NULL,
    project_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    component_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('held','committed','released','expired','recovered')),
    arrival_sequence INTEGER NOT NULL UNIQUE,
    acquired_at TEXT NOT NULL,
    expires_at TEXT,
    resolved_at TEXT,
    unknown_consumption INTEGER NOT NULL DEFAULT 0 CHECK(unknown_consumption IN (0,1)),
    overrun INTEGER NOT NULL DEFAULT 0 CHECK(overrun IN (0,1))
);

CREATE TABLE resource_reservation_items (
    reservation_id TEXT NOT NULL REFERENCES resource_reservations(reservation_id),
    pool_id TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    requested_amount INTEGER NOT NULL CHECK(requested_amount > 0),
    granted_amount INTEGER NOT NULL CHECK(granted_amount > 0),
    observed_amount INTEGER,
    overrun_amount INTEGER NOT NULL DEFAULT 0 CHECK(overrun_amount >= 0),
    PRIMARY KEY(reservation_id, pool_id, resource_type),
    FOREIGN KEY(pool_id, resource_type) REFERENCES resource_pools(pool_id, resource_type)
);

CREATE TABLE reservation_liveness_evidence (
    evidence_id INTEGER PRIMARY KEY AUTOINCREMENT,
    reservation_id TEXT NOT NULL REFERENCES resource_reservations(reservation_id),
    owner_instance_id TEXT NOT NULL,
    nonce_or_generation TEXT NOT NULL,
    resolution TEXT NOT NULL CHECK(resolution IN ('live','provably-dead','ambiguous')),
    provenance TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);

CREATE TABLE reservation_events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
    reservation_id TEXT NOT NULL REFERENCES resource_reservations(reservation_id),
    event_type TEXT NOT NULL,
    actor TEXT NOT NULL,
    detail TEXT NOT NULL,
    occurred_at TEXT NOT NULL
);

CREATE INDEX reservation_state_expiry ON resource_reservations(state, expires_at);
CREATE INDEX reservation_owner_instance ON resource_reservations(owner_instance_id, nonce_or_generation);
