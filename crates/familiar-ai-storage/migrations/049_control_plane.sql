CREATE TABLE control_plane_installation (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    installation_id TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);

CREATE TABLE control_plane_projects (
    project_id TEXT PRIMARY KEY,
    root_path TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    concurrency_ceiling INTEGER NOT NULL DEFAULT 1 CHECK (concurrency_ceiling > 0),
    state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','paused','archived')),
    last_claim_sequence INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE control_plane_executions (
    execution_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES control_plane_projects(project_id),
    idempotency_key TEXT NOT NULL UNIQUE,
    mode TEXT NOT NULL CHECK (mode IN ('detached','attached','foreground_only')),
    priority INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL CHECK (state IN ('queued','running','paused','completed','failed','cancelled','foreground_ended','ambiguous_live_orphan')),
    stage TEXT,
    attempt INTEGER NOT NULL DEFAULT 0,
    claim_generation INTEGER,
    worker_identity TEXT,
    checkpoint_id TEXT,
    command_json TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT
);
CREATE INDEX control_plane_queue_order ON control_plane_executions(state, project_id, priority DESC, created_at, execution_id);

CREATE TABLE control_plane_events (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    execution_id TEXT NOT NULL REFERENCES control_plane_executions(execution_id),
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX control_plane_event_stream ON control_plane_events(execution_id, cursor);

CREATE TABLE control_plane_capability_sessions (
    session_hash TEXT PRIMARY KEY,
    client_class TEXT NOT NULL CHECK (client_class IN ('operator','observer','mcp','worker','internal')),
    project_id TEXT REFERENCES control_plane_projects(project_id),
    execution_id TEXT REFERENCES control_plane_executions(execution_id),
    attempt INTEGER,
    worker_id TEXT,
    authority_json TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE TABLE control_plane_claim_generations (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    generation INTEGER NOT NULL
);
INSERT INTO control_plane_claim_generations(singleton, generation) VALUES (1, 0);

CREATE TABLE control_plane_divergences (
    divergence_id TEXT PRIMARY KEY,
    project_id TEXT REFERENCES control_plane_projects(project_id),
    execution_id TEXT REFERENCES control_plane_executions(execution_id),
    kind TEXT NOT NULL,
    detail TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);

CREATE TABLE control_plane_pending_gates (
    gate_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES control_plane_projects(project_id),
    execution_id TEXT NOT NULL REFERENCES control_plane_executions(execution_id),
    requested_by_session_hash TEXT NOT NULL REFERENCES control_plane_capability_sessions(session_hash),
    request_json TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','approved','rejected')),
    decided_by TEXT,
    created_at TEXT NOT NULL,
    decided_at TEXT
);

CREATE TABLE control_plane_workers (
    worker_identity TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES control_plane_executions(execution_id),
    attempt INTEGER NOT NULL,
    pid INTEGER NOT NULL,
    process_start_identity TEXT NOT NULL,
    launch_token_hash TEXT NOT NULL,
    owner_generation INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('launching','running','adopted','exited','ambiguous')),
    exit_reason TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT
);
