2026-05-12 — Author agent had the same working directory bug as
reviewers — running from main/ instead of the worktree.
→ Resolved: cd to worktree in cmd_run_bare and cmd_run_local before
run_session_loop. Also disable commit.gpgsign in worktree git config
so agents can commit without hardware key interaction.
