2026-06-08 — Work-model candidate worktrees now belong beside the source
checkout, not under `.factory/work/`. Durable Work Item JSON, review
artifacts, merge artifacts, and operator state still live under
`.factory/work/`, but Task workspace refs should use portable sibling
paths such as
`../work-<work-item-id-byte-len>-<work-item-id>-<attempt-id>`. Cleanup,
dashboard, and merge follow-up work should treat `.factory/work/` as
durable state and managed sibling worktrees as transient execution roots.

→ Resolved: Adopted. Candidate worktrees live as siblings (e.g., work-21-claude-auth-detection-attempt-2 with 21 = byte length of work item id) and durable state under .factory/work/{items,attempts,tasks,merge-candidates,artifacts}. The split is the current model and was demonstrated in every Work Item run this session.
