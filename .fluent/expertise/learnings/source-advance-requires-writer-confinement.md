---
name: source-advance-requires-writer-confinement
description: Accept clean source advances only under the applied Writer sandbox while preserving the candidate and its original base
metadata:
  type: architecture
---

The Writer runs in a candidate worktree while the source checkout can advance
independently. The completion guard may accept a changed source HEAD only when
the applied Writer sandbox denies source writes, the source remains on the same
named branch, both product statuses are clean, the old HEAD is an ancestor of
the new HEAD, and the relevant product bytes match the resulting checkout.
Exclude `.fluent/work` runtime state from the product comparison.

Derive this exception from the retained sandbox profile at every Writer return,
including provider retries and handoff corrections. A requested sandbox setting
or clean status alone does not establish the boundary. Without that profile,
retain strict snapshot equality even for a clean descendant advance. Branch
switches, rewrites, and rejected product drift pause the Task and preserve the
source for inspection.

Accepting an independent landing does not rebase the candidate, replace its
recorded base, or advance its host-owned baseline. Later integration checks
decide whether that unchanged candidate can land against the newer source.

The production-route regressions in `src/work_task_executor.rs` include
`writer_source_checkout_clean_fast_forward_does_not_pause`,
`writer_source_checkout_unsafe_drift_needs_user`, and
`writer_source_checkout_unsandboxed_advance_needs_user`. They check candidate
ancestry and persisted baselines as well as Task outcome and preserved source.

Related: [[first-writer-binds-committed-source]],
[[source-validation-uses-checkout-bytes]].
