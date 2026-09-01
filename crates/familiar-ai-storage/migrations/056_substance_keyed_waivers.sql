-- FAM-BUG-044: reviewer-chosen finding ids rotate between attempts, so a
-- durable human waiver keyed only by finding_id never matches the newest
-- attempt's open finding, and the FOREIGN KEY to review_findings blocked
-- save_cycle's findings rewrite outright. Rebuild the table: no FK to
-- review_findings (waivers must survive finding replacement), and a
-- finding_substance column (category + evidenced paths/checks hash) so a
-- waiver covers the same claim under any id. Legacy rows backfill with the
-- empty substance and keep matching by exact id.
CREATE TABLE review_finding_waivers_new (
    waiver_id TEXT PRIMARY KEY,
    cycle_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    finding_substance TEXT NOT NULL DEFAULT '',
    actor TEXT NOT NULL CHECK(length(trim(actor)) > 0),
    reason TEXT NOT NULL CHECK(length(trim(reason)) > 0),
    created_at TEXT NOT NULL,
    FOREIGN KEY(cycle_id) REFERENCES review_cycles(cycle_id),
    UNIQUE(cycle_id, finding_id)
);
INSERT INTO review_finding_waivers_new(waiver_id,cycle_id,finding_id,actor,reason,created_at)
    SELECT waiver_id,cycle_id,finding_id,actor,reason,created_at FROM review_finding_waivers;
DROP TABLE review_finding_waivers;
ALTER TABLE review_finding_waivers_new RENAME TO review_finding_waivers;
