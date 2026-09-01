# PRD-038: Multi-Repository Have-At-It Acceptance

This is the repository-agnostic product acceptance proof: design documents
in, one approved decomposition, unattended bounded execution, and an exact
morning report, exercised against materially different repositories. It is
implemented entirely as real integration tests over real Familiar library
code — no network, no live model, no shell mocks beyond fake
`CodingAgent`/`CommandRunner` implementations that the rest of the test
suite already uses.

## Fixture repositories

`tests/fixtures/` holds two checked-in repository templates with different
languages, dependency identities, and (as configured by the onboarding
answers used against them) different backlog layouts:

- `repo-rust-cli/` — Rust/Cargo, dependency `serde`, onboarded with the
  canonical layout (`docs/prds` / `docs/prds/done`).
- `repo-node-service/` — Node.js/npm, dependency `express`, onboarded with a
  materially different layout (`planning/backlog` /
  `planning/backlog/done`).

Tests copy these trees into a fresh temp directory and `git init` them, so
onboarding discovery always reads real filesystem content, never a
synthetic string.

## Where each acceptance criterion is proven

| # | Acceptance criterion | Proof |
|---|---|---|
| 1 | At least two fixture repositories with different languages, layouts, and dependency identities complete onboarding without code changes | `crates/familiar-ai-daemon/tests/multi_repo_acceptance.rs::rust_cli_repository_onboards_without_code_changes`, `::node_service_repository_onboards_without_code_changes`; MCP-side echo in `crates/familiar-ai-mcp/tests/multi_repo_acceptance.rs::rust_cargo_repository_backlog_surface_matches_the_generic_shape`, `::node_npm_repository_backlog_surface_matches_the_generic_shape` |
| 2 | The planner drafts a valid dependency-ordered batch and records one human batch approval plus its warrant | `crates/familiar-ai-daemon/tests/multi_repo_acceptance.rs::planner_drafts_a_dependency_ordered_batch_under_one_human_approval_and_a_warrant` |
| 3 | Independent branches execute concurrently while dependencies and shared scopes serialize correctly | `::two_independent_branches_execute_concurrently` (measured peak concurrency across two disjoint-scope PRDs); `::failure_strands_only_its_own_dependent_and_survives_restart` (a declared dependency correctly blocks its dependent); `::shared_scope_without_a_declared_dependency_still_serializes` (an *undeclared* shared-scope conflict still serializes two independent PRDs, distinct from dependency-graph serialization) |
| 4 | Worker routing includes a cheap or local task and independent strong review | `::worker_routing_selects_a_cheap_local_worker_and_an_independent_strong_reviewer` (exercises the real `run::resolved_worker_plan` composition-root function); MCP-side separation evidence in `crates/familiar-ai-mcp/tests/multi_repo_acceptance.rs::control_plane_worker_surface_can_progress_and_escalate_but_never_approve` |
| 5 | Injected failures strand only affected work and survive supervisor restart | `::failure_strands_only_its_own_dependent_and_survives_restart`: PRD-001 fails deterministically, its dependent PRD-002 is never attempted, unrelated independent PRD-003 still runs normally, and a simulated supervisor crash is recovered via `worktree::recover_incomplete` without touching unrelated already-terminal attempts |
| 6 | Manual delivery stops at reviewed PR; explicit PoC mode self-approves only within its warrant; review-gated mode preserves separation evidence | `::manual_reviewed_pr_delivery_stops_before_merge_or_deploy`, `::poc_self_approval_delivers_within_its_warrant_and_never_targets_production`, `::review_gated_automatic_delivery_preserves_separation_evidence` |
| 7 | Reports state work, blockers, cost, cache behavior, human gates, and recovery | `::report_states_work_blockers_cost_cache_human_gates_and_recovery` (asserts every morning-report section is present) |
| 8 | Repeating the proof creates no duplicate claims, PRs, merges, or deployments | `::repeating_a_completed_delivery_never_repeats_pr_merge_or_deploy_effects`, `::repeating_batch_approval_creates_no_duplicate_planner_record`; MCP-side in `crates/familiar-ai-mcp/tests/multi_repo_acceptance.rs::repeating_backlog_completion_through_mcp_creates_no_duplicate_completion` |

## Running the proof

```sh
cargo test -p familiar-ai-daemon --test multi_repo_acceptance
cargo test -p familiar-ai-mcp --test multi_repo_acceptance
```

Both suites use only fake `CodingAgent`, `CommandRunner`, and planner-agent
implementations local to the test files; they touch no network and invoke
no real model or provider CLI.
