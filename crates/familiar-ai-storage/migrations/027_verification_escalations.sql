ALTER TABLE driver_attempts ADD COLUMN escalated_from_sequence INTEGER NULL;
ALTER TABLE driver_attempts ADD COLUMN escalation_reason TEXT NULL;

CREATE UNIQUE INDEX driver_attempts_single_escalation_idx
ON driver_attempts(session_id, escalated_from_sequence)
WHERE escalated_from_sequence IS NOT NULL;
