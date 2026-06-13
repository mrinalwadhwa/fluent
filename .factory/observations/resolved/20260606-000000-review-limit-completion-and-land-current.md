2026-06-06 — Review-limit completion and land currently disagree. Run
`20260606-180035` reached the maximum review rounds, fixed the latest
blocking architecture finding, committed a clean worktree, and the
session loop accepted the run as complete. `factory land` still rejected
it because the top-level `review-architecture.md` artifact from the
previous review round still had verdict `fail`. Recovery archived that
stale review artifact before landing. Factory should make this contract
explicit: either review-limit completion must rerun or clear stale
top-level verdicts before completing, or `land` must understand an
accepted review-limit completion marker. The source of truth for review
verdicts should live in the review subsystem, not leak as ambiguous
durable run state. In addition to tightening that contract, Factory may
need to raise or tune the review-round limit so useful runs do not hit
the ceiling while they are still making productive progress.
surface where any observing human can act on a cue. That likely needs a
permission model over time, so different humans can be allowed to
observe, triage, approve runs, restart runs, resolve needs-user items,
or land changes at different levels.

Learning capture should happen at every level of this system, not only
as an after-the-fact human note. Individual agents can record local
learnings from their session: codebase facts discovered, wrong
assumptions corrected, tool failures, review misunderstandings, and
what they would do differently. A run-level observer or reporting agent
can synthesize learnings across author and reviewer sessions: why the
run looped, which artifacts were missing, which tests or environments
behaved differently, and what should change in Factory process. Across
runs, the land command or merge queue can detect recurring patterns and
turn them into durable observations, expertise, behavior mappings,
checks, or decisions. Any time work bubbles back up to the human
operator or coordinating agent, that is itself a signal: Factory lacked
enough automation, context, policy, artifact quality, or recovery logic
to finish autonomously, and the event should be captured as input for
improving the system. The agent focused on learning capture should look
at full transcripts from multiple agents and, when synthesizing broader
patterns, multiple runs. Final reports and handoffs are useful summaries,
but full transcripts preserve false starts, reviewer/author
disagreements, tool failures, and repeated human interventions that can
disappear from polished artifacts. Learning synthesis should cite which
transcripts or runs informed it and distinguish single-run lessons from
cross-run patterns.

One review role or expertise file should also nudge changes toward
vocabulary consistency. This may belong in architecture expertise,
documentation expertise, or both: architecture can check whether a term
matches the domain model and component boundaries, while documentation
can check whether user-facing names stay consistent across behaviors,
docs, tests, commands, and dashboard copy. The design question is how to
make this a gentle review signal rather than churn over harmless wording.

→ Resolved: Obsolete. Slice 3 removed the legacy 'factory land' path and the top-level review-verdict mismatch surface this observation described. Work-model 'factory work merge' uses MergeCandidate review_state on the candidate itself; no leak from per-round review artifacts. The observation file also contained content from other observations (permission model, learning capture, vocabulary consistency) — those were unrelated paragraphs picked up during the monolithic-to-per-file split migration. The review-round-tuning kernel that survives is too speculative to design without data showing it's an active problem.
