---
name: serialized-reviewer-cache-admission
description: Reviewer warm-cache admission is a project-wide serialized transaction that reclaims, accounts, checks capacity, and then copies; any admission failure starts the reviewer cold without pausing the Attempt
metadata:
  type: architecture
---

Reviewer build caches are an optional acceleration, never a requirement for a
Review Task. Admission therefore holds the project-level reviewer-cache lease
across the full transaction: reclaim caches from terminal Review Tasks, account
for the remaining managed bytes, inspect free space, compare the prospective
total against both configured limits, and copy only after those checks pass.
Releasing the lease between accounting and copying permits concurrent reviewers
to each admit against the same stale total and exceed the project budget.

Treat every admission prerequisite as fail-cold. Invalid limits, unavailable
capacity information, accounting failures, reclamation failures, and an
exceeded limit must name the Reviewer and reason, then allow the review to run
without a warm cache. The same transaction applies to canonical caches written
by a `prepare-pre-review` hook: retain the hook's noncanonical output, but
remove its unadmitted managed cache directories. Related:
[[lease-acquire-types-contention-vs-infrastructure]],
[[canonical-reviewer-cache-cleanup]].
