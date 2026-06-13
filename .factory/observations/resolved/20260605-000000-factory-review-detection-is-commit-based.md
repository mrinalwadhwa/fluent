2026-06-05 — Factory review detection is commit-based. During run
`20260605-193223`, an author wrote valid implementation changes and
marked the run complete, but left the worktree dirty. Factory compared
`main..HEAD`, saw no committed diff, skipped reviews, and produced a
no-code-changes report. The session loop should require or verify a clean
committed worktree before `complete`, or Factory should detect dirty
worktrees and fail/needs-user instead of skipping reviews.
→ Resolved: cfba7c3 (dirty worktrees count as changed so completed
author work cannot bypass review because it was not committed)
