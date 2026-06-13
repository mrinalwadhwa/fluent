2026-06-06 — Stale run artifacts need a first-class cleanup policy rather
than manual deletion. Cleanup should happen where the Factory state
resides: the source worktree's `.factory/runs` registry and its
registered git worktrees. It should not be modeled as ordinary author
work inside an isolated run worktree, because that worktree only carries
its own copied run state. Landed and reported runs should remain
queryable but should not dominate the default dashboard view.
Complete and landed stale runs need a `factory cleanup` command that
preserves the cleanup reason in the source Factory state and removes
registered git worktrees safely. Superseded planned runs, failed smoke
runs, and other stale artifacts still need an explicit
abandoned/superseded status or archive marker outside the current
cleanup command scope.
The leftover Codex smoke worktrees (`20260605-codex-installed-smoke`,
`20260606-codex-installed-ca-smoke`, and
`20260606-codex-installed-seatbelt-smoke`) point at commits already
contained in `main`, but the curation run could not remove their sibling
worktree directories because Git could not validate those paths under
the run sandbox's filesystem permissions.
→ Resolved: `factory cleanup` preserves run directories, writes
`cleaned.md` for complete and landed runs, removes only registered git
worktrees, skips unregistered paths, and keeps cleaned runs behind
actionable dashboard runs.
