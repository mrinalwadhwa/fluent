---
name: provider-admission-before-role-reservation
description: Acquire shared provider capacity before reserving Writer, reviewer, or Learner state, then hold it across the complete logical run
metadata:
  type: architecture
---

Provider capacity is an admission boundary, not execution state. Run read-only
provider and sandbox preflight first, acquire capacity, and only then reserve a
Task as executing or Learning as in progress. While capacity is unavailable,
the Task stays `Planned` and the Learner stays unreserved, so waiting consumes no
Writer round, Task retry, or Learner run.

Use process-held advisory locks because schedulers and direct Fluent commands can
run in separate processes and projects. Partition the user-wide slots by
provider, pair each with a project slot that may impose a lower ceiling, and
never retain a partial pair while probing another slot. Only lock contention is
a wait condition; propagate path and lock failures. Prove cross-process behavior
with a child process on macOS.

Hold the lease across the complete logical role run: provider-owned auth and
rate-limit recovery, Task-owned generic retries, and Learner schema repairs.
Dropping the guard then releases capacity on success, failure, cancellation, or
unwind; OS process exit releases it after a crash. Keep retry ownership separate:
when the coder exhausts rate-limit recovery, return a typed terminal error that
the Task retry predicate excludes.

Related: [[atomic-task-start-reservation]],
[[lease-acquire-types-contention-vs-infrastructure]],
[[macos-flock-child-process-test-pattern]],
[[terminal-coder-errors-bypass-retry-budget]].
