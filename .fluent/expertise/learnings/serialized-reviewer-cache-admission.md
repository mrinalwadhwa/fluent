---
name: serialized-reviewer-cache-admission
description: Candidate-keyed reviewer-cache admission is a project-wide serialized transaction that reclaims unreferenced caches, accounts, checks capacity, and then copies; failures start the reviewer cold
metadata:
  type: architecture
---

Reviewer build caches are an optional acceleration, never a requirement for a
Review Task. Admission therefore holds the project-level reviewer-cache lease
across the full transaction: compute the candidate commits still referenced by
nonterminal Work, reclaim only unreferenced shared caches, retire eligible legacy
per-Task caches, account for the remaining managed bytes, inspect free space,
compare the prospective total against both configured limits, and copy into the
candidate-keyed cache only after those checks pass.
Releasing the lease between accounting and copying permits concurrent reviewers
to each admit against the same stale total and exceed the project budget.

Treat every admission prerequisite as fail-cold. Invalid limits, unavailable
capacity information, accounting failures, reclamation failures, and an
exceeded limit must name the Reviewer and reason, then allow the review to run
without a warm cache. The same transaction applies to canonical caches written
by a `prepare-pre-review` hook: relocate an admitted canonical cache to the
shared candidate cache, retain the hook's noncanonical output in the artifact,
and remove any unadmitted managed cache directories. Related:
[[lease-acquire-types-contention-vs-infrastructure]],
[[canonical-reviewer-cache-cleanup]].
