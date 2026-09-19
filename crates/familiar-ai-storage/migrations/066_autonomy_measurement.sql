-- PRD-085: unattended stall taxonomy and autonomy measurement.
--
-- The stall vocabulary itself is a closed, configured Rust table (see
-- familiar-ai-storage::repos::session_rollup), not schema — a stall class
-- is a contract on values already durable in driver_attempts.retained_reason.
-- What this migration adds is the indexing the new bounded-window autonomy
-- query needs to scan actor-tagged rows without a full table scan per PRD:
-- the same three durable sources PRD-083 and PRD-084 already write
-- (scope_decisions, backlog_status_events, execution_checkpoint_events),
-- filtered to the human:-prefixed writes that make an assisted completion
-- distinguishable from an unattended one.

CREATE INDEX scope_decisions_human_window_idx
    ON scope_decisions(repository_key, prd_id, decided_at, actor);

CREATE INDEX backlog_status_events_human_window_idx
    ON backlog_status_events(repository_key, prd_path, changed_at, actor);

CREATE INDEX execution_checkpoint_events_recorded_idx
    ON execution_checkpoint_events(recorded_at);
