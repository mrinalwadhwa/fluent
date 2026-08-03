---
name: release-suite-scoped-process-guard
description: Tester-owned release settlement compares host process inventories around each guarded suite to report surviving Fluent or provider processes
metadata:
  type: testing
---

Per-fixture cleanup cannot cover abrupt termination or an uncovered fixture, so
each release Tester command enables `reject_process_leaks`. Tester creates the
fixture root and snapshots host process state before and after the sandboxed
suite, settles briefly for normal teardown, and fails if a newly alive Fluent,
Claude, Codex, or Pi process has a working directory beneath that root.

The diagnostic must identify the PID, classified process kind, and matched root.
Scope processes by process metadata such as their current working directory, not
by a broad temporary-directory prefix or an argv substring: normal Fluent
commands do not necessarily contain their project root in argv, and broad roots
can attribute unrelated concurrent processes to the suite.

Keep the deterministic collector seam inside the trusted Tester component for
tests. A host that denies process-table access produces a Tester harness error;
the project command cannot provide a substitute inventory. This guard is an
independent backstop; it complements rather than replaces
[[release-fixture-process-group-ownership]].
