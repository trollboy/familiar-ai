CREATE TABLE backlog_recovery_events_v2 (
    status_event_id INTEGER PRIMARY KEY,
    action TEXT NOT NULL CHECK(action IN ('release', 'manual_complete_override', 'recorded_complete')),
    reason TEXT NOT NULL CHECK(length(trim(reason)) > 0),
    FOREIGN KEY(status_event_id) REFERENCES backlog_status_events(event_id)
);

INSERT INTO backlog_recovery_events_v2 (status_event_id, action, reason)
SELECT status_event_id, action, reason FROM backlog_recovery_events;

DROP TABLE backlog_recovery_events;

ALTER TABLE backlog_recovery_events_v2 RENAME TO backlog_recovery_events;
