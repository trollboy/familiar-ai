-- PRD-099, amended 2026-09-19: the gate's verdict is local.
--
-- This is a locally running desktop application. Verification runs here, on
-- the machine doing the work, and the record of it belongs in Familiar's own
-- ledger rather than in a forge's check-run API. A verdict that requires a
-- network call to read is a verdict you cannot read offline, and it couples
-- verification to a vendor whose independence PRD-101 is explicitly about.
CREATE TABLE gate_verdicts (
    commit_sha  TEXT PRIMARY KEY,
    verdict     TEXT NOT NULL CHECK(verdict IN ('green', 'red')),
    -- Which steps ran and how they ended, so a red verdict says what failed
    -- without needing the original terminal scrollback.
    detail      TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
