ALTER TABLE backlog_prds ADD COLUMN prd_suffix TEXT NULL CHECK(prd_suffix IS NULL OR (length(prd_suffix) = 1 AND prd_suffix GLOB '[a-z]'));
ALTER TABLE backlog_bootstrap_items ADD COLUMN prd_suffix TEXT NULL CHECK(prd_suffix IS NULL OR (length(prd_suffix) = 1 AND prd_suffix GLOB '[a-z]'));

CREATE INDEX backlog_prds_identity_order
    ON backlog_prds(repository_key, prd_number,
        CASE WHEN prd_suffix IS NULL THEN 1 ELSE 0 END, prd_suffix, prd_path);
