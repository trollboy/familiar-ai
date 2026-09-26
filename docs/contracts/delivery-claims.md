# Delivery Claims Contract

Autonomous-delivery status is computed from the delivery ledger, never inferred
from prose or Git history. Every claim in the README, roadmap, or an
after-action report must place one JSON record beside the prose:

```text
familiar-delivery-claim: {"repository_key":"/repo/.git","hosts":["linux-host"],"window_start":"2026-09-01T00:00:00Z","window_end":"2026-10-01T00:00:00Z","accepted_prds":1,"unattended_prds":1,"measured_cost_executions":1,"total_cost_executions":1}
```

The check requires the repository, a bounded half-open window, and the complete
host set. A local store can support only its named host; a project-wide claim
is refused, naming every missing host ledger. It recomputes accepted delivery
from `integrated_at`, unattended delivery from PRD-085 autonomy evidence, and
cost coverage from recorded attempt cost. Any disagreement fails the check.

An execution-outcome bug closure additionally carries both the exact query and
its recorded result. A narrative, screenshot, commit list, or audit summary is
context, not closure evidence. FAM-BUG-019 remains the migration example: its
multi-PRD delivery exit criterion cannot close without a query whose recorded
rows demonstrate that outcome.
