---
name: lock-ordering-across-subsystems
description: Release the queue lock before mutating the Work model; the codebase has a lock hierarchy across queue, Work model, lineage, candidate, and follow-up locks that must not be inverted
metadata:
  type: architecture
---

The codebase holds several independent file locks — the queue lock plus the Work model, lineage, candidate, and follow-up locks. Code that touches more than one must acquire them in a consistent order to avoid deadlock from lock inversion.

The concrete discipline the architecture reviewer checks: **release the queue lock before mutating the Work model.** For corrective follow-up mutations, acquire locks in this order: follow-up operation (when present), root lineage, a specific Work Item, then queue reconciliation. Release the lineage and Work locks before queue reconciliation. Replay retains its outer operation lock through `ensure_dispatch` so no same-operation processor observes a partially completed journal; queue code must therefore never acquire a Work, lineage, candidate, or follow-up lock. For example, the scheduler drops the queue lock before `ensure_bound_attempt` mutates the Work model, while corrective promotion drops its lineage lock before `ensure_dispatch`.

A related invariant makes some ledger mutators safe without a generation check: recovery transitions (`requeue_active`, `reconcile_active`, `block_active`, `cancel_active`) mutate the latest dispatch under the queue lock without a token-based generation check, unlike the owning worker's `with_matching_active`. This is only safe because recovery runs solely in the single elected coordinator's loop and reconciliation bails out while a bound-Attempt lease is live — so no worker and recovery ever write the same dispatch concurrently. A future contributor who introduces a second concurrent writer of the ledger must restore the generation check; the single-coordinator + live-lease invariant, not a generation check, is what currently keeps recovery correct.

Related: [[backward-compatible-serde-fields]]
