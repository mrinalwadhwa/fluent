---
name: post-land-effects-are-idempotent-and-land-safe
description: A completed land is durable; post-land side effects run only after merge, are keyed by deterministic ids for at-most-once replay, and never undo the land on failure
metadata:
  type: architecture
---

Once a Merge Candidate reaches merge status `merged`, the land is durable and must never be undone by anything that runs afterward. Side effects that follow a land — materializing learner handoffs into Observations, deriving corrective Work, charging lineage, enqueuing — obey three invariants the architecture reviewer enforces:

1. **Land-gated.** Nothing materializes before the merge is durable. The land hook (`process_landed_batch` in `src/follow_up.rs`, fired from `src/work_merge_executor.rs`) runs on both the fresh-land path and the already-merged early-return path, through a single entry point — there is no second materialization path.

2. **Idempotent via deterministic keys.** Each effect is keyed by a deterministic id (Observation id per follow-up, derived Work Item id, journal receipt) so a land retry, recovery, or journal replay produces each Observation, Work Item, lineage charge, and queue entry **at most once**. A resumable journal under `.fluent/work/follow-ups/land-<digest>/`, where the digest identifies the Work Item and Merge Candidate, preserves completed stages; a completed resume clears any recorded failure. Digests are computed over canonical (sorted-key) JSON and recomputed from parsed structs, so verification is independent of on-disk formatting. A resolved Observation is never reopened; a matching id with conflicting provenance is rejected.

3. **Land-safe on failure.** A malformed/origin-mismatched handoff or a failure at any effect stage (Observation, Work, or queue) keeps merge status `merged`, still reports the merged commit as successful, and records a **retryable follow-up-processing failure** on the candidate naming the first incomplete stage plus a next action (e.g. `merge-candidate land`). Re-running `fluent merge-candidate land` on the merged candidate resumes handoff-only — it does not resolve workspaces, rebase, or move the target branch. When a slice defers the durable failure projection (early rounds only warned to stderr), record that deferral explicitly so the reviewer does not flag it.

A related consequence for cleanup: `fluent cleanup` must retain an origin (Work Item, Attempt, Merge Candidate, worktree, artifacts) while its landed-learning recovery is still live — a failed retryable Learner record, an incomplete post-land journal, or a pending imported post-land operation — and may only reap once that processing completes. See [[needs-user-not-terminal-for-cleanup]] for the parallel rule that cleanup reapability is gated on lifecycle state, not just terminal status.

To prove the land-safe/idempotent behavior in tests without fault hooks in production code, obstruct a stage's deterministic output path — see `.fluent/expertise/testing/patterns/inject-stage-failure-via-filesystem-obstruction.md`.

Related: [[keep-architecture-doc-in-sync]], [[needs-user-not-terminal-for-cleanup]], [[backward-compatible-serde-fields]]
