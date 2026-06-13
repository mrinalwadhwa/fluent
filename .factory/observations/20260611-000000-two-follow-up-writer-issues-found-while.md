2026-06-11 — Two follow-up writer issues found while fixing migration
tests. (1) `candidate_has_failure` in `src/work_merge_executor.rs` only
checked for `MergeCandidateMergeStatus::Failed`, so when the merge loop
recorded `NeedsUser` after follow-up budget exhaustion, the outer
fallback recorder overwrote it back to `Failed`. Fixed in this session
by widening the predicate to match both `Failed` and `NeedsUser`. (2)
The merge follow-up writer requires the mock to produce a brand new
commit each invocation (`if new_head == baseline_commit { bail }`), but
several shell mocks always wrote the same filename, so the writer's
second cycle errored on "did not produce any new commits" and the merge
ended with status=failed instead of going through the budget-exhaustion
path. Both issues only surface once `MAX_MERGE_FOLLOWUP_WRITES_PER_INVOCATION`
is non-zero — worth a project-level reviewer that flags new mock claude
scripts that don't distinguish writer/reviewer roles or don't make
progress on each invocation.
