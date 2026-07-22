---
name: reserved-phase-terminal-finalizer
description: Once a phase reserves durable state (Task Executing, learning in-progress), its whole failure path must funnel through one finalizer that persists authoritative state before any handoff/artifact — never a raw ? or .expect() out of the reserved body
metadata:
  type: architecture
---

Every reserved-phase execution path — Writer, Tester, Reviewer, Learner, and the
rebase Task, plus the round-cap / uncertain-review pauses — must route its
*entire post-reservation body* through a single terminalizing/pausing finalizer.
Once a phase has reserved durable state, raw fallible or panicking steps
(`os::check_prerequisites_for(...)?`, `credential::inject_credentials()?`,
`build_coder_sandbox(...)?`, prompt-render `.expect(...)`, `run_captured`, a
head lookup) must not propagate out through `?`/panic, because any of them
returning early leaves the durable Task stranded `Executing` (or a learning
record orphaned) for outer recovery. The architecture reviewer treats a
reserved body that escapes its finalizer as a blocking B7-class hole.

The doctrine the reviewers call "make durable state authoritative":

- **Persist authoritative state before any auxiliary artifact.** Write the
  terminal/pause Work state *before* writing the operator handoff, before
  advancing a merge-candidate tip, and notify last. A crash between an exposed
  handoff/candidate and a later durable write is the exact failure this closes.
  `interpret_reviews` persists the `RoundCap`/`Uncertain` suspend first and
  attaches the handoff reference in a later mutation; `finalize_learning` writes
  the handoff last.
- **Reserve a crash-observable in-progress state for artifact-writing phases
  that are not Tasks.** The Learner persists `AttemptLearning::in_progress`
  before its coder runs and before any handoff; the land/retry gate treats it as
  pending via `is_pending()` (`InProgress` ⇒ retryable), so a crash mid-run is
  retried under the land lock rather than mistaken for a completed run. This
  generalizes the Task-start reservation to non-Task phases.
- **One finalizer per phase.** `run_reserved_rebase` → `terminalize_rebase_failure`
  (rebase), `finalize_review_outcome` (Reviewer), `finalize_learning` (Learner).
  A terminal-state-write failure is attached as secondary context, never allowed
  to mask the primary.

When you add or change a reserved-phase path, keep this shape: reserve, then run
the whole body inside one finalizer whose result persists the authoritative state
before any artifact. See [[compose-typed-failure-precedence]] for how the
finalizer combines the primary and secondary typed errors. Related:
[[atomic-task-start-reservation]], [[needs-user-not-terminal-for-cleanup]],
[[host-evidence-writes-use-exclusive-create]].
