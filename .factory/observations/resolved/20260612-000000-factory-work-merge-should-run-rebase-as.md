2026-06-12 — `factory work merge` should run rebase as an agentic
step, not a pure `git rebase` followed by hook chain. The rebase
itself should be handled by an agent so trivial conflicts (additive
edits to `.factory/observations-resolved.md`, log files, append-only
docs) get resolved inline. No reviewers needed after the rebase step —
the rebase agent's output IS the merge candidate to fast-forward,
gated by `check-pre-merge` / `fix-pre-merge` as today. Concrete
trigger: during the parallel speed test for `fargate-teardown-command`,
the candidate rebased onto a main that had just received a sibling Work
Item's commits plus an auto-generated `post-merge-review-fix-main-*`
commit. The conflict in `.factory/observations-resolved.md` was purely
additive; `git rebase` bailed before any hook ran. Manual recovery
touched five JSON files for one trivial conflict.
→ Resolved: `factory work merge` now invokes an agent for the rebase
step via `TaskKind::Rebase`. The agent runs `git rebase <target>`,
resolves trivial conflicts inline, and returns the new candidate-tip
SHA. Factory regenerates post-rebase provenance (candidate_commit,
write task output.commit, attempt artifact paths) from the rebased
tip. When the agent cannot resolve conflicts, the Merge Candidate
transitions to `needs-user` with the diagnostic attached.
