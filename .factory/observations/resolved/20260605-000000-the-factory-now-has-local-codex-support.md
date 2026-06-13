2026-06-05 — The factory now has local Codex support via the Coder
abstraction: `--coder codex` / `FACTORY_CODER=codex` launches
`codex exec --json --cd <worktree>` and records the selected coder
in run state. This unblocks local no-sandbox runs for Codex. Remaining
agent-support work: verify sandboxed Codex, add Fargate Codex support,
and consider whether Pi or other agents need different prompt/session
behavior beyond the current Coder trait.

→ Resolved: Milestone now in git history. Listed followups all addressed: sandboxed Codex works, Fargate Codex support landed today (Work Item fargate-codex-support at 31f6b6c), and 'Pi or other agents' is speculative future Coder additions to be captured per agent if/when added.
