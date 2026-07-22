---
name: atomic-task-start-reservation
description: A Task start that mutates durable state and launches a coder must run read-only preflight, then a single lock-held reservation that honors the precedence boundary, then CAS rollback — never mark Executing while discarding the transition verdict
metadata:
  type: architecture
---

Starting a Task is destructive: it marks the Task `Executing`, clears its
output, persists the Work Item, and launches a coder/tester child. The precedence
boundary `transition_attempt` exists to stop a Task from reviving an Attempt a
peer has already taken terminal (a `NeedsUser`/`TranscriptPump`/`Auth` pause).
The architecture reviewer treats the start path as the *last line of defense*:
between the loop-level `reject_terminal_attempt` and a given Task's start, a
parallel peer can pause the Attempt, so the start must re-check precedence and
honor the verdict. Calling `transition_attempt` and then unconditionally mutating
anyway — discarding the returned boolean — is a blocking finding: it persists an
inconsistent durable state (Attempt `NeedsUser` while a Task is `Executing`) and
launches an unwanted coder on a paused Attempt.

The enforced three-phase start protocol (`work_task_executor.rs`):

1. **`plan_task_start`** — a read-only preflight. Validate identity, kind, and
   `Planned` status, run the executor-specific `validate`, and reject a
   peer-taken terminal via a typed `StartRejected` error. It commits nothing, so
   a rejection and any deterministic setup error both leave the aggregate
   byte-identical. The durable reservation is deferred until after setup so those
   failures never need a rollback.
2. **`reserve_task_start`** — the *only* place a Task is marked `Executing`, done
   inside one `WorkModelStore::mutate_work_item` transaction (one flock-held read
   → reducer → validate → write). It re-checks `Planned` status and calls
   `transition_attempt`; a `false` return (or non-`Planned` status) yields
   `Decision::Rejected`, the reducer mutates nothing (a durable no-op), and the
   function returns `StartRejected`. It also captures a field-complete
   `ReservationReceipt` (the exact prior and written Task/Attempt fields).
3. **`with_reservation_rollback`** — runs side-effectful setup after the
   reservation; on failure it CAS-reverts via `rollback_reservation`, which
   restores the Task and the Attempt *independently*: revert the Task whenever it
   still equals what the reservation wrote (so a CAS mismatch never orphans an
   `Executing` Task), but restore the Attempt only when it is still exactly what
   the reservation wrote (a peer that took it terminal is preserved).

Every start call site propagates `StartRejected` with `?`, so a rejection stops
the invocation with no Task mutation and no child launch. Completion sites are
safe discarding the verdict because they only persist their own terminal and let
`transition_attempt` preserve a peer's; start sites are not, because they perform
destructive mutations and a launch. `is_start_rejected` distinguishes this typed
terminal from a truthful setup failure.

When adding or changing a Task-start path, keep this shape: read-only preflight →
single lock-held reservation that honors `transition_attempt` → receipt-based CAS
rollback. Do not collapse it into a check-then-mutate that ignores the boundary's
return value. Related: [[lock-ordering-across-subsystems]],
[[host-evidence-writes-use-exclusive-create]],
[[needs-user-not-terminal-for-cleanup]].
