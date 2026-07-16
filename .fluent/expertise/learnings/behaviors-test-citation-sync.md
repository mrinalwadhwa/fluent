---
name: behaviors-test-citation-sync
description: Test renames must update all Test: citations in behaviors.md in the same commit
metadata:
  type: convention
---

When a test function is renamed, every `Test:` citation in `documentation/behaviors.md` that references the old name must be updated in the same commit. The documentation reviewer treats stale citations as a blocking finding — they are the same class of defect as a missing test, because they break the traceability chain from behavior statement to verifying test.

A single test may be cited by multiple behavior statements (e.g., B3 and B4 in the same section). Search for all occurrences of the old name, not just the first.

Related: [[test-names-match-assertions]]
