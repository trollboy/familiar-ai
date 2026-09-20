# Escalation surface

**PRD-102.** How Familiar says it needs you, and what each signal is allowed
to mean.

## The problem this exists for

On 2026-09-19 a four-PRD wave left two PRDs awaiting a human decision —
PRD-090 blocked on a scope finding at 12:39, PRD-100 needing review at 22:47.
The owner learned about both from a chat transcript, hours later. For that
whole span the tray icon sat there looking exactly as it looks when nothing
is wrong, while the daemon behind it held the answer in a table it already
queried.

Zero bother means never interrupting unnecessarily. It does not mean never
speaking. A blocked PRD is the one moment the autopilot genuinely needs its
operator.

## The three signals

| Signal | Says | Where |
|---|---|---|
| **Badge** | how many PRDs are waiting | composed onto the tray icon |
| **Notification** | a *new* gate appeared, and why | desktop, once per stop |
| **Window** | the list, and the decision | gates view |

**The menu signals; the window decides.** The tray menu carries the count and
a way in. Every actual decision is made in the window, so there is one place
decisions happen and the menu stays a signal rather than a workspace. The
cost is one extra click on the common path, accepted deliberately.

## Zero is silent

No badge, no notification, no tooltip change. `compose_with_count(base, 0)`
returns the base icon **byte for byte**, and there is a regression asserting
exactly that.

A surface that lights up when nothing is wrong is a surface its operator
stops reading, and the entire value of the badge is that it means something.

## Announce once, per stop

A gate is announced the first time it is seen and never again. Identity is
the PRD plus its distinct stop reasons, so:

- the same PRD stopped the same way, seen again — silent;
- the same PRD stopped a **new** way — announced, because that is new
  information;
- a gate decided and later recurring — announced again, because the memory is
  rebuilt from the live set each tick rather than accumulated. Nothing is
  permanently suppressed by a recollection of an old stop.

## A failed notification is never a missing gate

The badge, the list and the decision path are read from
`pending_human_gates`. The notification is a courtesy that tells the operator
to go and look. If the desktop notification service is absent or refuses —
normal on a headless box — the failure is logged and everything else still
reports the truth.

This is the same discipline the verification gate applies to unknown
outcomes: absence of evidence is never evidence.

## Decisions carry attribution, because the ledger refuses them otherwise

`validate_recovery_attribution` rejects an empty actor, rejects an empty
reason, and requires the actor in `human:<identity>` form.

The tray's Release and Force-complete buttons passed `String::new()` for
both, so **every click failed validation** — two of the three decision
buttons had never worked. The prompt is not a nicety; it is the difference
between the button working and not.

- **Re-drive** changes no durable verdict and needs no justification.
- **Release** discards retained work, and **Force-complete** marks a PRD done
  with its gates unsatisfied. Both are overrides. Both ask why, refuse a
  blank answer in the dialog rather than sending it to be rejected, and
  record `human:<user>` as the actor.

A decision made from the tray is indistinguishable in the ledger from one
made at a terminal. That is the point: the surface changes, the record does
not.

## What holds the truth

Nothing in the UI. Count and list are read on refresh, so a decision made
elsewhere, a restarted daemon, or a wave finishing while the window is open
all converge on the next read rather than on a cached number.

The badge redraws only when the count changes — composing a 512×512 icon
every tick to produce identical bytes is work nobody asked for.

## Costs no new dependencies

The badge is composed with the `image` crate the tray already uses to decode
its icon, using a hand-coded 3×5 bitmap font for eleven glyphs. Notification
uses `gio`, which arrives with the `gtk` dependency already present. This is
the same constraint the native settings and dashboard windows were built
under.

## Out of scope

What constitutes a gate, and the taxonomy of stop reasons (PRD-085). Any
change to how decisions are adjudicated once recorded — PRD-080 owns that
shape and this reuses it. Notification on any platform other than the Linux
tray this runs on, which is a successor with its own acceptance.
