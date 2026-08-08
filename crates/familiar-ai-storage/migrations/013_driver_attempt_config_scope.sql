-- PRD-026: a review verdict must be attributable to the policy that produced
-- it, so each attempt records which scope its repository-shaped configuration
-- came from. Existing rows predate repository scoping and are therefore global.
ALTER TABLE driver_attempts
    ADD COLUMN review_scope TEXT NOT NULL DEFAULT 'global'
    CHECK (review_scope IN ('global', 'repository'));

ALTER TABLE driver_attempts
    ADD COLUMN execution_context_scope TEXT NOT NULL DEFAULT 'global'
    CHECK (execution_context_scope IN ('global', 'repository'));
