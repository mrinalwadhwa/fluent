2026-06-09 — Work-model behavior reviews do not have a first-class
`behaviors.diff.md` artifact like legacy runs did. During targeted
follow-up review work, behavior reviewers had to infer new behaviors
from `documentation/behaviors.md`, the candidate diff, and Work Item
planning context. The Work review prompt or artifact model should make
the Work Item behavior increment explicit so behavior reviewers can stay
within their no-source-code boundary without guessing from docs.
→ Resolved: 56d8dae. Work review Tasks and merge-time behavior
reviewers now receive a "Work behavior review input" prompt section from
`WorkItem.planning_context.behaviors`, or an explicit message that no
Work behavior increment was provided. `review-behaviors` now treats
legacy `.factory/runs/[run-id]/behaviors.diff.md` as a legacy-only input
and tells Work-model reviewers to use the prompt context and exact Work
artifact path.
