2026-06-19 — Pi's writer behavior on multi-step Work Items
defaults to "edit everything in the worktree, run cargo
build/test until passing, then commit it all at the end."
Claude's writer behavior on the same kind of Work Item commits
incrementally — one commit per plan.md step.

Concrete instance observed: on a Work Item with a 7-step
plan.md, Claude produced 7 commits during its first writer
Task (one per step, made progressively as each step completed).
Pi on the same Work Item produced 0 commits for ~90 minutes
while accumulating roughly 800 lines of edits across 19 files,
then made a single monolithic commit at the end.

The difference matters for two reasons:

1. **Robustness against context loss.** A writer that accumulates
   many edits before committing loses everything if context is
   compacted, an OAuth-style interruption fires, or any fatal
   error happens. A writer that commits per step loses at most
   one step's work.

2. **Trajectory observability.** Per-step commits give reviewers,
   future learner phases, and benchmark comparisons a structured
   record of how the writer worked. A monolithic commit collapses
   the trajectory.

The completion contract Pi received from `prompts/work-author.md`
says: "Commit all Task output in the writable workspace before
marking the Task complete." This is satisfied by a single
end-of-task commit. The progress.md writer protocol added later
does say to commit per plan step, but Pi appears to interpret
the protocol's `Make the code changes` + `Git commit the code
changes` instructions loosely — it accepts the spirit (work gets
committed eventually) without honoring the step granularity.

Possible nudges to try:

- Tighten the progress.md protocol wording in
  `prompts/work-author.md` to be more explicit about the step
  boundary: "After each plan step's code lands, commit before
  reading plan.md again to find the next `- [ ]` item. Do not
  accumulate multiple steps' changes into one commit."
- Add an example to the protocol showing the commit-per-step
  rhythm.
- Update the completion contract from "Commit all Task output
  before marking the Task complete" to "Commit each plan step's
  changes as it lands; the workspace should never carry more than
  one in-progress step's edits."
- Consider adding a hint that Pi specifically benefits from
  per-step commits to recover from context compactions (since
  Pi's context-management pattern depends on disk state surviving
  across context resets).

This is worth a small prompt-tuning Work Item. The change is
non-functional (cosmetic-only to the resulting code) but
materially improves Pi's reliability on long Work Items.
