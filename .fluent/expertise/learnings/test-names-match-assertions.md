---
name: test-names-match-assertions
description: Test function names must describe the behavior the test actually asserts, not what a behavior statement claims
metadata:
  type: convention
---

Test function names must accurately describe the observed behavior the test asserts. If a test asserts `invocations == 3` (one initial run + one retry + one final check), the name should say `_after_one_retry`, not `_without_retrying`.

When a test name contradicts its assertions, it masks semantic mismatches between behavior statements and actual system behavior. The test reviewer checks that names match assertions and will block on contradictions.

If correcting a test name reveals that the corresponding behavior statement text is also inaccurate, and fixing the statement is out of scope, note it for a follow-up work item rather than leaving it silently inconsistent.

Related: [[behaviors-test-citation-sync]]
