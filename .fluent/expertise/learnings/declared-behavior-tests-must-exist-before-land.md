---
name: declared-behavior-tests-must-exist-before-land
description: Every Test: reference declared in behaviors.md must resolve to a real, passing test before landing — a green suite does not substitute for a missing production-boundary regression, and behaviors/tests/architecture reviewers all block on the gap
metadata:
  type: testing
---

A green test suite is not evidence that a Work Item is complete. The plan's
completion rule is that no behavior may lack a production-boundary regression, so
every `Test:` reference declared in `documentation/behaviors.md` must resolve to
a test that exists in the tree *and* passes in the tester results — independent of
how many other tests pass.

In this project the behaviors, tests, and architecture reviewers each verify this
independently: they `grep` for every declared test name in `src/` and `tests/`
and cross-check the tester-results ids. A declared reference with no backing
definition is a blocking finding *even when the full suite is green* (a round
shipped 1376 passing tests while five declared references were undefined). The
absence is read as structural evidence that the production path itself is
unfinished, not merely untested.

When you declare a `Test:` reference, either land the backing test passing in the
same candidate, or — if the test is intentionally dropped — remove/replace the
reference and add an `Untestable:` justification. Do not leave a dangling
reference expecting the green suite to cover for it. This is the same
traceability chain [[behaviors-test-citation-sync]] protects against stale
renames; here the reference never had a backing test at all. Related:
[[route-tests-drive-real-launch-wiring]], [[test-names-match-assertions]].
