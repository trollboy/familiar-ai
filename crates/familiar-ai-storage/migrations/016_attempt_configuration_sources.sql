ALTER TABLE driver_attempts ADD COLUMN review_configuration_source TEXT NOT NULL DEFAULT 'global'
    CHECK(review_configuration_source IN ('global', 'repository'));
ALTER TABLE driver_attempts ADD COLUMN execution_context_configuration_source TEXT NOT NULL DEFAULT 'global'
    CHECK(execution_context_configuration_source IN ('global', 'repository'));
