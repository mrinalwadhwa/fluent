---
name: fixture-diagnostics-survive-temporary-state
description: Retain bounded public scheduler diagnostics outside temporary fixtures before assertions or cleanup remove their state
metadata:
  type: testing
---

Scheduler integration fixtures keep queue, Work, execution, and service state
under temporary project roots. A timeout assertion can unwind those roots before
anyone can inspect why progress stopped. Collect the owned child's output and
save relevant public state under the test's persistent output directory before
asserting failure or removing the fixture.

Use the shared scheduler failure snapshot helper in `tests/binary.rs`. Bound
both traversal and retained bytes, prioritize the affected Work and queue
records, and retain exit status and output tails. Select public files explicitly:
service homes and Task artifact trees also contain private worker homes,
credentials, and caches. Refuse symlinks in parent components as well as at the
copied file; recursively copying the artifact tree crosses that boundary.

Verify that diagnostics remain readable after the temporary project drops,
exclude private files and symlinked credentials, and preserve process-group
settlement. Retaining diagnostics must not extend the behavior's deadline or
replace its outcome assertions.

Related: [[fixture-output-capture-precedes-polling]],
[[release-fixture-process-group-ownership]].
