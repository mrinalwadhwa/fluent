---
name: per-target-outcomes-need-mixed-target-tests
description: Per-target CLI summaries need a mixed-target regression that proves an outcome from one target cannot appear in another target's report
metadata:
  type: testing
---

When a command selects multiple installation targets and reports one summary for
each, derive each summary only from that target's outcome. A single invocation
with both a target that takes the exceptional path and an ordinary target is the
regression boundary: assert that the exceptional target reports its fact and
that the ordinary target does not.

Single-target exceptional tests and all-ordinary tests do not prove this
scoping. They both pass if a later aggregation accidentally applies an outcome
from any selected target to every printed summary. Drive the real multi-target
CLI route and locate each summary by its target path before asserting the
target-specific wording.

Related: [[negative-test-cross-product-coverage]],
[[managed-skill-installation-ownership]].
