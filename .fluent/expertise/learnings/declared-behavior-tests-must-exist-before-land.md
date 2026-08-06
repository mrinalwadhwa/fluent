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

Fluent now enforces this traceability at two host-owned candidate gates. Before
review, the deterministic gate resolves every approved reference against its
committed project-relative path and native test identifier. The behaviors
reviewer then checks that the named test directly exercises the behavior; code
reviewers run before the final Tester and treat Writer-focused verification as
advisory. After the final Tester, the host gate requires every reference to match
a passing structured Rust-test or shell-script identity for the exact candidate.
A declared reference with no backing definition therefore blocks before review,
and a defined reference without passing final evidence blocks before Learning,
even when many unrelated tests are green.

When you declare a `Test:` reference, either land the backing test passing in the
same candidate, or — if the test is intentionally dropped — remove/replace the
reference and add an `Untestable:` justification. Do not leave a dangling
reference expecting the green suite to cover for it. This is the same
traceability chain [[behaviors-test-citation-sync]] protects against stale
renames; here the reference never had a backing test at all. Related:
[[route-tests-drive-real-launch-wiring]], [[test-names-match-assertions]].
