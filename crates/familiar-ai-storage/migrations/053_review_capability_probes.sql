CREATE TABLE review_capability_probes (
    spec_identity TEXT PRIMARY KEY REFERENCES worker_specs(spec_identity),
    structured_output INTEGER NOT NULL CHECK(structured_output IN (0,1)),
    native_tool_calling INTEGER NOT NULL CHECK(native_tool_calling IN (0,1)),
    protocol TEXT NOT NULL,
    runtime_version TEXT NOT NULL,
    provenance TEXT NOT NULL CHECK(provenance IN ('probed','observed')),
    probed_at TEXT NOT NULL
);
