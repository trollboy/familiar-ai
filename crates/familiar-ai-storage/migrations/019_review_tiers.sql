CREATE TABLE review_tier_selections (
    cycle_id TEXT PRIMARY KEY,
    tier TEXT NOT NULL,
    selecting_rule TEXT,
    selection_json TEXT NOT NULL,
    FOREIGN KEY(cycle_id) REFERENCES review_cycles(cycle_id)
);
