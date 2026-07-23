---
name: readiness-gate-at-selection-not-permanent-skip
description: A shared readiness predicate must gate at every projection including auto-merge selection, and a retryable gate must withhold selection (retry later), never select-then-fail-then-mark a permanent skip
metadata:
  type: architecture
---

When a readiness condition is expressed as one shared predicate
(`Attempt::learning_advancement_readiness` →
`WorkItem::attempt_learning_advancement` →
`MergeCandidate::validate_advancement`), it must be consumed at **every**
projection that acts on readiness, not just the boundary the change was written
for. The enumerated projections here are the review-pass gate, Merge Candidate
validation, land (`work_merge_executor.rs`), **auto-merge selection**
(`find_ready_candidate` in `auto_merge.rs`), and the status/dashboard display
labels. Adding the gate at the land boundary while leaving `find_ready_candidate`
selecting on the raw `status/review_state/merge_state` tuple is a liveness
regression the architecture reviewer blocks.

The non-obvious rule is **where** a retryable gate belongs. A candidate that is
merely waiting on an in-progress or relaunchable Learner is *not yet* ready but
*will* become ready. Such a condition must be enforced at **selection**:
`find_ready_candidate` returns `None` so the candidate is simply not chosen this
poll and is retried on a later one. It must **not** be enforced by selecting the
candidate, letting the land/merge gate fail, and then calling a permanent-freeze
projection. `mark_auto_merge_skipped` (`auto_merge_skipped = Some(true)`) is a
**permanent** exclusion — once set, `find_ready_candidate` skips the candidate
forever, even after the Learner succeeds. Driving a permanent-freeze projection
from a transient/retryable condition strands the candidate.

Discipline when you add or move a readiness gate: identify every projection that
reads the same readiness and wire the shared predicate into each, and confirm
that any permanent-exclusion flag is only ever set by a genuinely terminal
condition — never by a gate that a later poll could pass. Prove it with negative
tests over each non-ready state (in-progress, handoff-pending, failed, absent).
Related: [[compose-typed-failure-precedence]],
[[post-land-effects-are-idempotent-and-land-safe]].
