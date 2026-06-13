2026-06-06 — Parallel parent recovery needs to be merge-phase aware. Run
`20260606-queues-cleanup-reporting` produced useful child commits, but
the parent failed during child landing because the source worktree had
new dirty observation edits. After the observation was committed, a
parent resume restarted the child plan instead of resuming only the
failed merge/land phase. That reset child metadata, damaged the `1-1`
branch pointer, and then failed all relaunched children under nested
`sandbox-exec` with `sandbox_apply: Operation not permitted`. Factory
should prevent dirty source worktrees before parent landing, preserve
completed child state, support merge-only recovery for failed parallel
parents, and avoid relaunching completed child work when the only failed
step is parent-side merge/land.

→ Resolved: Obsolete. Legacy parent/child run surface (parent restarts child plan, resets child metadata) doesn't exist in the Work model. The general principles it raised are already realized: clean source worktree before merge (ensure_clean_worktree in work_merge_executor), preserve completed state (split JSON storage per Work Item/Attempt/Task), merge-only recovery (Merge Candidates separate from Attempts), don't relaunch completed work (Task.status respected by the Attempt loop).
