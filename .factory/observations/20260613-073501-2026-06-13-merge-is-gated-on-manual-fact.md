2026-06-13 — Merge is gated on manual factory work merge invocation
after reviews pass. When an Attempt finishes its review phase and the
Merge Candidate is ready, the candidate sits indefinitely until a
human (or automation) triggers the merge command.

Concrete incident: behavior-tests-task Attempt-1 finished reviews at
~00:55 on 2026-06-13 and sat idle until 07:12 when the next merge
invocation happened — a 6h 17m gap that was pure scheduling latency,
not agent work. The user's laptop also slept during this window
(clamshell sleep), so the situation compounded: nothing was watching
to invoke merge.

Auto-merge-when-reviews-pass would close the gap. Considerations:

- The Attempt loop already detects "reviews passed" and writes the
  Merge Candidate; one extra step ("invoke merge if no human
  override") completes the chain.
- Some Work Items want a human pause point (e.g., the user wants to
  inspect the candidate diff before fast-forwarding). An env var or
  per-Work-Item flag (auto_merge_on_reviews_passed: true) makes
  this opt-in.
- Failure modes: if the auto-merge agentic rebase fails (token
  expired, conflict the agent can't resolve), the Merge Candidate
  transitions to needs-user — same surface today's manual flow
  produces. No new failure category.
- Could also add a watch process (similar to post-merge-review run)
  that polls for "reviews passed, candidate ready" and invokes
  merge.

This is the single biggest speed lever once the rest of the pipeline
is fast.
