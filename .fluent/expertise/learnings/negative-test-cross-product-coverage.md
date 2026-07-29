---
name: negative-test-cross-product-coverage
description: Negative tests for a multi-condition gate must hold all other conditions fixed while varying only the dimension under test — independent variation misses cross-product gaps
metadata:
  type: testing
---

When a validation gate has multiple independent required conditions
(e.g. `status_ok AND review_state == Passed`), a focused negative test must
exercise the invalid cross-products, not just each invalid dimension in isolation.

The common gap: each dimension is tested by resetting from the valid baseline to
an invalid value. When testing the exception branch (e.g. `NeedsUser`/`HostSandbox`),
the test may start from a non-exception state, flip the review state to invalid,
and confirm rejection — but it never combines the exception state with a non-passed
review. This left the host-sandbox exception admitting unreviewed candidates because
the negative test only exercised:

- (other-status, non-passed-review) — a different code path entirely
- (NeedsUser, HostSandbox, Passed) — valid combination, accepted

It never tested:

- (NeedsUser, HostSandbox, NotReviewed) — should be rejected
- (NeedsUser, HostSandbox, Uncertain) — should be rejected
- (NeedsUser, HostSandbox, Failed) — should be rejected

The fix: for each "special" entry point (exception, recovery path, early return), write
negative cases that hold the exception condition constant while systematically varying
every other required conjunct through its invalid values. This is especially important
when the exception was added to relax one condition — the invariance of the remaining
conditions is exactly what the exception must not silently break. Related:
[[exception-must-not-widen-conjunctive-invariant]].
