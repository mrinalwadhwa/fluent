2026-06-06 — Clarify the boundary between conversation-state edits and
delegated run execution. The agent that is actively collaborating with a
user should be allowed to write discussion artifacts directly: briefs,
observations, behavior drafts, approaches, plans, and lightweight
curation. That keeps the human planning loop fast and avoids pushing
work that can be done directly into unnecessary runs. The same agent
should not meddle with live run state: run branches, worktrees, statuses,
session artifacts, child metadata, and landing state belong to the run
system unless the user explicitly approves recovery. To keep `main`
available as a stable rebase and merge target, direct conversation edits
should happen on a lightweight discussion branch or worktree whenever
active runs or parent landing could overlap with those edits.
