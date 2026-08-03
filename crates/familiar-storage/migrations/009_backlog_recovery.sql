CREATE TABLE backlog_recovery_events (
    status_event_id INTEGER PRIMARY KEY,
    action TEXT NOT NULL CHECK(action IN ('release', 'manual_complete_override')),
    reason TEXT NOT NULL CHECK(length(trim(reason)) > 0),
    FOREIGN KEY(status_event_id) REFERENCES backlog_status_events(event_id)
);
