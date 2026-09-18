-- PRD-099: a merge past a red or absent gate is possible, but never quiet.
--
-- The point is not to make override impossible; it is to make it impossible
-- to do without leaving a record naming who did it, which commit, and why.
-- Same shape as PRD-012's backlog_recovery_events and the review finding
-- waivers: an actor and a non-empty reason, both enforced by the schema
-- rather than by the caller remembering.
CREATE TABLE gate_overrides (
    override_id TEXT PRIMARY KEY,
    commit_sha  TEXT NOT NULL CHECK(length(trim(commit_sha)) > 0),
    -- The verdict being overridden, recorded so the record still means
    -- something after the run it refers to has aged out of the forge.
    verdict     TEXT NOT NULL CHECK(verdict IN ('red', 'absent', 'unreadable')),
    actor       TEXT NOT NULL CHECK(length(trim(actor)) > 0),
    reason      TEXT NOT NULL CHECK(length(trim(reason)) > 0),
    created_at  TEXT NOT NULL
);

CREATE INDEX gate_overrides_commit ON gate_overrides(commit_sha);
