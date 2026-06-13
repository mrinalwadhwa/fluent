2026-06-05 — The run tab shows "[planned]" for runs that are actively
executing because the tab reads source run status instead of live
worktree status.
→ Resolved: 1fc4b8c (run tabs use cached live status from the same
live_dir source as the header)
