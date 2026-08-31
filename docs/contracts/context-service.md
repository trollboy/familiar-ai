# Context service contract

The daemon owns one symbol map per canonical Git repository. Watcher create and
modify events replace only the named file region and its outgoing reference
edges; remove and rename events remove only their named regions. Ambiguous
events mark the map stale. Unsupported or unreadable files and repositories
without watcher coverage are returned as named partial coverage.

Serialization is UTF-8, path ordered, and length-delimited (`FAMILIAR-REPOMAP-v1`).
Each file is an independent JSON region containing its content hash, symbol
definition lines, signatures, and reference edges. Identical regions have
identical bytes across restarts. Locations are one-based current-worktree lines.

`context.repository_map` exposes the representation to MCP clients. Familiar
prompt injection is disabled by default (`repository_map_enabled = false`) and
may only become effective from an approved per-repository execution-context
configuration. Disabled injection uses the legacy prompt renderer exactly.

Token sink reports are measured exclusively from `usage_observations`, grouped
by stage, worker, and the five persisted token categories. Effect reports join
those rows to audited injection-state execution facts and return on, off, and
signed delta values; missing observations are not synthesized as zero-valued
observations.
