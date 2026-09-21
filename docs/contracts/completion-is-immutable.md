# Completion is immutable

**A PRD marked complete is never reversed. Incompleteness is named in a
successor PRD.**

## The rule

When a PRD says do A, B and C, and only A and B were done, the answer is not
to un-complete it. The answer is a new PRD that names the failure and builds
C.

There is deliberately no command that moves a PRD out of `completed`. Every
recovery action — `release`, `complete`, `record-complete`,
`approve-and-complete` — moves toward completion or sideways within it. That
asymmetry is the contract, not an oversight, and it should not be "fixed".

## Why

**The ledger is append-only or it is not evidence.** A status that can be
walked backwards is a status that can be quietly corrected, and a record that
can be quietly corrected cannot be cited. This project has already paid for
that once: a closure note claimed two PRDs "completed hands-off through clean
independent review and merge-queue integration" and the database disagreed.
Reversing a status would have hidden that; leaving it and writing the
correction is what surfaced it.

**A reversal loses the finding.** The interesting artefact is not that a PRD
was wrong — it is *which* criterion was not met and what evidence says so. A
successor PRD carries that forward with the finding ids attached. An
un-complete throws it away and leaves a PRD that looks like it was never
attempted.

**Partial completion is normal and should be cheap to record.** Making it a
new, small, well-scoped PRD means the gap gets the same treatment as any
other work: a manifest, acceptance criteria, a review. Making it a status
edit means it gets none of those.

## What a successor PRD must carry

- **The predecessor and why it was marked complete.** Not an accusation — the
  review that passed it, and the disposition it recorded.
- **The finding ids it inherits**, with their evidence refs, so the successor
  proves its claim by citation rather than by restating it. A reviewer that
  already looked at this should not have to look twice.
- **Only the unmet part.** A successor that re-opens settled ground is a
  re-run wearing a new number.

## Worked example

PRD-090 (CLI surface design pass) was marked `completed` on 2026-09-21 after
a clean independent review — disposition `ReadyForHumanApproval`, stop reason
`CleanReview`. That review recorded four non-blocking findings, two of which
say acceptance criteria were only nominally satisfied:

- `f2-ac5-leaf-set-not-compared` — AC5 asked for a before/after comparison of
  the full reachable leaf set; a hand-written partial list shipped.
- `f3-alias-regression-does-not-assert-target` — AC3 asked that each alias be
  shown to resolve to its target; an assertion about a printed notice
  shipped.

Non-blocking findings do not stop a cycle, and should not: a reviewer that
halts on every observation stops being worth running. But "did not block" is
not "was done". PRD-103 is the successor, and PRD-090 stays completed.

## What this does not excuse

A PRD is not a place to park work that was simply skipped. The successor
exists because a review found a gap and recorded it, which means the gap is
evidenced. Splitting a PRD to make it look finished, with no finding behind
the split, is the failure this rule would otherwise enable.
