---
name: release-suite-scoped-process-guard
description: Declared release commands compare root-scoped process inventories before and after each suite to report surviving Fluent or provider processes
metadata:
  type: testing
---

Per-fixture cleanup cannot cover abrupt termination or an uncovered fixture, so
each declared release command runs through the release-test process guard. The
guard snapshots process state before and after the suite, settles briefly for
normal teardown, and fails if a newly alive Fluent, Claude, Codex, or Pi process
has a working directory beneath that suite's temporary fixture roots.

The diagnostic must identify the PID, classified process kind, and matched root.
Scope processes by process metadata such as their current working directory, not
by a broad temporary-directory prefix or an argv substring: normal Fluent
commands do not necessarily contain their project root in argv, and broad roots
can attribute unrelated concurrent processes to the suite.

Keep a deterministic inventory injection seam for hostile hosts that deny
process-table access, but also test live root-scoped discovery. This guard is an
independent backstop; it complements rather than replaces
[[release-fixture-process-group-ownership]].
