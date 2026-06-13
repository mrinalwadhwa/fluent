2026-05-12 — setup_run_worktree reuses an existing branch at its old
commit instead of current HEAD, causing stale code on retries.
→ Resolved: when branch exists, reset it to current HEAD with
git branch -f before checking out. Fixed in both shell script and
Rust binary. Test added. First run completed using the Rust binary.
