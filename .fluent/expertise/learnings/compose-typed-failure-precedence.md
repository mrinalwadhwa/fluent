---
name: compose-typed-failure-precedence
description: A phase finalizer that faces a coder/pump primary plus a cleanup/confinement secondary must compose them by a fixed precedence — resumable pump/auth primary pauses (NeedsUser); a pure integrity/confinement failure is a hard Failed — and keep the primary downcastable, never stringified
metadata:
  type: architecture
---

When a reserved-phase finalizer holds more than one typed failure at once — a
coder/transcript-pump primary alongside a workspace confinement or cleanup
failure, or a terminal-state-write failure — it must *compose* them, not let one
mask or flatten the other. Cleanup erasing a coder/pump failure is the
failure-masking class this subsystem exists to close.

The enforced precedence (`finalize_review_outcome`, `terminalize_rebase_failure`):

- A **resumable typed primary** — a `TranscriptPumpError` or an auth error —
  outranks a workspace confinement/cleanup failure: the Attempt pauses
  (`NeedsUser`, classified from the pump primary, e.g. `PauseKind::TranscriptPump`),
  and the cleanup failure is retained as *secondary context*.
- A **pure workspace-confinement / integrity failure** with no resumable primary
  is a hard `Failed`, with the coder error attached as secondary. Integrity
  failures stay hard failures.
- A **terminal-state-write failure** is always composed as secondary context and
  never masks the primary.

The typed primary must survive by `downcast_ref`, not be collapsed to a string —
tests assert the primary is still downcastable and that the pause classifies from
it, not from the secondary. This is why a Learner failure carries a typed
`LearningFailureKind` (`TranscriptPump` vs `Generic`) on its durable record
rather than only a diagnostic string: a transport fault must stay discoverable
through recovery.

This precedence is a genuine, non-obvious design choice — reconciling "retain the
pump primary alongside cleanup failures" against "integrity failures are hard
failures." Record it in `decisions.md` when you touch it, so a future contributor
does not have to re-derive it. Related: [[reserved-phase-terminal-finalizer]],
[[terminal-coder-errors-bypass-retry-budget]], [[needs-user-not-terminal-for-cleanup]].
