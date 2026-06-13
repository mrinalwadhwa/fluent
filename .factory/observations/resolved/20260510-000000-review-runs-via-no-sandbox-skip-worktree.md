2026-05-10 — Review runs via --no-sandbox skip worktree creation.
Author commits directly to main.
→ Resolved: cmd_run_bare creates worktree when in a git repo (local).
Skips on Fargate where there's no git repo.
