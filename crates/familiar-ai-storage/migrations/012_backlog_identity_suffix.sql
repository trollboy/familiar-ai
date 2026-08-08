-- PRD-025: a PRD identity is a number plus an optional single lowercase letter
-- marking an epic child. Existing rows are all suffixless, so they migrate to
-- NULL deterministically and their identity, ordering, and display are
-- unchanged in every observable.
ALTER TABLE backlog_prds
    ADD COLUMN prd_suffix TEXT NULL
    CHECK (prd_suffix IS NULL OR (length(prd_suffix) = 1 AND prd_suffix GLOB '[a-z]'));

ALTER TABLE backlog_bootstrap_items
    ADD COLUMN prd_suffix TEXT NULL
    CHECK (prd_suffix IS NULL OR (length(prd_suffix) = 1 AND prd_suffix GLOB '[a-z]'));

-- The identity order, in SQL, matching the domain order exactly: ascending by
-- number, and within one number every suffixed identity strictly before the
-- unsuffixed umbrella.
CREATE INDEX backlog_prds_identity_order
    ON backlog_prds(repository_key, prd_number, prd_suffix IS NULL, prd_suffix);
