-- PRD-087: identity and event-sequence invariants.
--
-- 1. `execution_checkpoint_events.sequence` makes the per-checkpoint
--    occurrence sequence (previously encoded only inside the event_id
--    string, `{checkpoint_id}:{phase}:{n}`) a real, indexed column. A
--    UNIQUE index turns "no identifier collision across occurrences"
--    (FAM-BUG-039) from a convention every writer had to remember into a
--    constraint the database itself enforces.
-- 2. `identity_invariant_tolerances` is the durable record this PRD's
--    design requires: enforcing an invariant may expose existing rows that
--    predate it, and those rows are migrated forward WITH A RECORD, never
--    silently accepted or deleted. The backfill below writes one row here
--    for the schema-level migration; recovery paths that tolerate a
--    pre-invariant row at runtime (e.g. review recovery accepting a cycle
--    written before `repository_key` existed, FAM-BUG-032) append to the
--    same ledger.

ALTER TABLE execution_checkpoint_events ADD COLUMN sequence INTEGER;

-- Backfill from rowid insertion order: `execution_checkpoint_events` has no
-- other total order guaranteed unique per checkpoint (two events recorded
-- in the same transaction can share `recorded_at`).
UPDATE execution_checkpoint_events
SET sequence = (
    SELECT COUNT(*) FROM execution_checkpoint_events AS earlier
    WHERE earlier.checkpoint_id = execution_checkpoint_events.checkpoint_id
      AND earlier.rowid < execution_checkpoint_events.rowid
)
WHERE sequence IS NULL;

CREATE UNIQUE INDEX execution_checkpoint_events_sequence_idx
    ON execution_checkpoint_events(checkpoint_id, sequence);

CREATE TABLE identity_invariant_tolerances (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    invariant TEXT NOT NULL,
    table_name TEXT NOT NULL,
    row_key TEXT NOT NULL,
    detail TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);

CREATE INDEX identity_invariant_tolerances_invariant_idx
    ON identity_invariant_tolerances(invariant);

INSERT INTO identity_invariant_tolerances(invariant, table_name, row_key, detail, recorded_at)
SELECT 'checkpoint-event-sequence-allocator', 'execution_checkpoint_events', 'schema-backfill',
       'backfilled sequence column from rowid insertion order for ' || COUNT(*) ||
       ' row(s) written before PRD-087 introduced the allocator',
       strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
FROM execution_checkpoint_events;
