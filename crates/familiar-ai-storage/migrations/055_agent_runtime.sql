-- PRD-058: the Familiar-owned raw-model agent loop. Every table here is
-- append-only (no_update/no_delete triggers, matching migrations 030/041/
-- 054) so the write-ahead tool journal and execution evidence can never be
-- silently rewritten after the fact.

-- One row per inference submission. Every submission is its own globally
-- unique billable attempt with its own PRD-064 reservation; a retry mints a
-- new row rather than reusing one. `ambiguous` marks a timeout whose
-- completion is unknown (usage for that attempt stays pending, never zero).
CREATE TABLE agent_runtime_attempts (
    attempt_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES execution_history(execution_id),
    reservation_id TEXT,
    provider_request_id TEXT,
    provider_idempotency_key TEXT,
    ambiguous INTEGER NOT NULL DEFAULT 0 CHECK(ambiguous IN (0,1)),
    created_at TEXT NOT NULL
);

CREATE INDEX idx_agent_runtime_attempts_execution ON agent_runtime_attempts(execution_id, created_at);

-- Write-ahead tool journal: an intent row must exist and be durable before
-- the corresponding tool call executes. `call_id` is scoped to one
-- execution (a model-issued tool-call identity), never reused for a
-- different call within that execution.
CREATE TABLE agent_runtime_tool_intents (
    intent_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES execution_history(execution_id),
    call_id TEXT NOT NULL,
    capability TEXT NOT NULL CHECK(capability IN (
      'read-file','search-list','run-command','apply-edit',
      'report-progress','submit-evidence','request-escalation'
    )),
    argument_hash TEXT NOT NULL,
    side_effect_class TEXT NOT NULL CHECK(side_effect_class IN ('read-only','idempotent-write','destructive')),
    recorded_at TEXT NOT NULL,
    UNIQUE(execution_id, call_id)
);

CREATE INDEX idx_agent_runtime_tool_intents_execution ON agent_runtime_tool_intents(execution_id);

-- Result rows follow intent rows. An intent with no matching result means
-- the call's outcome is unknown: resume policy (never re-run a destructive
-- call in that state) is enforced by the host reading this table, not by
-- storage itself.
CREATE TABLE agent_runtime_tool_results (
    result_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES execution_history(execution_id),
    call_id TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK(outcome IN ('succeeded','failed')),
    result_hash TEXT,
    failure_detail TEXT,
    recorded_at TEXT NOT NULL,
    UNIQUE(execution_id, call_id),
    CHECK ((outcome = 'succeeded') = (result_hash IS NOT NULL)),
    CHECK ((outcome = 'failed') = (failure_detail IS NOT NULL))
);

CREATE INDEX idx_agent_runtime_tool_results_execution ON agent_runtime_tool_results(execution_id);

-- One row per completed or terminated loop run. `offered_tools_json` and
-- `calls_json` are bounded audit summaries; the journal tables above remain
-- the authoritative record tool execution and resume decisions are made
-- from. Never carries a prompt, a model response, source code, or raw tool
-- output — only capability ids, dispositions, and content hashes.
CREATE TABLE agent_runtime_evidence (
    evidence_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES execution_history(execution_id),
    prompt_template_version TEXT NOT NULL,
    worker_spec_identity TEXT NOT NULL,
    worker_empirical_version TEXT NOT NULL,
    offered_tools_json TEXT NOT NULL,
    calls_json TEXT NOT NULL,
    stop_reason TEXT NOT NULL CHECK(stop_reason IN (
      'completed','iteration-ceiling','token-or-context-ceiling','budget-stop',
      'timeout','cancelled','provider-failure','fatal-tool-refusal',
      'invalid-structured-output'
    )),
    stop_reason_detail_json TEXT,
    iterations INTEGER NOT NULL,
    resume_conversation_messages INTEGER NOT NULL,
    resume_journal_high_water_mark INTEGER NOT NULL,
    recorded_at TEXT NOT NULL
);

CREATE INDEX idx_agent_runtime_evidence_execution ON agent_runtime_evidence(execution_id, recorded_at);

CREATE TRIGGER agent_runtime_attempts_no_update BEFORE UPDATE ON agent_runtime_attempts BEGIN SELECT RAISE(ABORT, 'agent runtime attempts are append-only'); END;
CREATE TRIGGER agent_runtime_attempts_no_delete BEFORE DELETE ON agent_runtime_attempts BEGIN SELECT RAISE(ABORT, 'agent runtime attempts are append-only'); END;
CREATE TRIGGER agent_runtime_tool_intents_no_update BEFORE UPDATE ON agent_runtime_tool_intents BEGIN SELECT RAISE(ABORT, 'tool journal intents are append-only'); END;
CREATE TRIGGER agent_runtime_tool_intents_no_delete BEFORE DELETE ON agent_runtime_tool_intents BEGIN SELECT RAISE(ABORT, 'tool journal intents are append-only'); END;
CREATE TRIGGER agent_runtime_tool_results_no_update BEFORE UPDATE ON agent_runtime_tool_results BEGIN SELECT RAISE(ABORT, 'tool journal results are append-only'); END;
CREATE TRIGGER agent_runtime_tool_results_no_delete BEFORE DELETE ON agent_runtime_tool_results BEGIN SELECT RAISE(ABORT, 'tool journal results are append-only'); END;
CREATE TRIGGER agent_runtime_evidence_no_update BEFORE UPDATE ON agent_runtime_evidence BEGIN SELECT RAISE(ABORT, 'agent runtime evidence is append-only'); END;
CREATE TRIGGER agent_runtime_evidence_no_delete BEFORE DELETE ON agent_runtime_evidence BEGIN SELECT RAISE(ABORT, 'agent runtime evidence is append-only'); END;
