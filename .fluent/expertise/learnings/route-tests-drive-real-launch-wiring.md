---
name: route-tests-drive-real-launch-wiring
description: A launch-route regression must drive the real phase launch path and fail if it drops or re-resolves the threaded value — a helper test that only asserts config/resolver layering does not verify the wiring and the tests reviewer blocks on it
metadata:
  type: testing
---

When a behavior requires that a phase's launch route threads a resolved value
(a `TranscriptCapture`, a resolved pump config) all the way into the coder, a
test that calls the resolver helper directly (e.g. `resolve_config_from(...)`)
and asserts config layering is *not* sufficient. A regression that dropped or
re-resolved the value on the actual route would still pass that helper test.

The tests reviewer draws an explicit resolver-vs-route distinction and blocks
when only the helper shape exists. A conforming route regression:

- drives the **real** launch route (`run_learner_with_coder`,
  `rebase_candidate_with_coder`), not a helper;
- injects a recording coder that captures what reached `run_captured`;
- asserts the resolved capture's transcript path *and* a distinctive resolved
  threshold (the tests use a sentinel project `console-preview-limit: 7777`)
  arrive verbatim;
- **fails if the route drops or re-resolves** the value — the failure
  sensitivity is the point.

This is the same "test the real path, not a copy of it" principle as
[[extract-logic-to-avoid-test-duplication]], applied to launch wiring: a helper
test verifies the resolver, only a route test verifies the route.
Related: [[public-api-surface-test]], [[declared-behavior-tests-must-exist-before-land]].
