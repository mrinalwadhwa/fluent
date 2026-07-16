---
name: test-fixtures-use-production-state
description: Test fixtures must use state values that production code actually creates, not values reachable only after later transitions
metadata:
  type: testing
---

The Work model has multiple interacting lifecycle state enums (`AttemptReviewState`, `AttemptStatus`, `MergeReviewState`, `MergeCandidateMergeStatus`). Test helper functions like `make_work_item_with_candidate` accept these as parameters, making it easy to construct states that production code never produces.

When testing a function that reads state (like `find_ready_candidate`), fixtures must use the state values that production code would have set at that point in the lifecycle. For example, a newly created `MergeCandidate` has `MergeReviewState::Pending` — the `Passed` value is only set after a successful merge. A readiness test that passes `Passed` into the fixture is testing an unreachable scenario and can mask real bugs.

The test reviewer checks that fixture state values match production creation paths and will flag unreachable combinations.

Related: [[test-names-match-assertions]]
