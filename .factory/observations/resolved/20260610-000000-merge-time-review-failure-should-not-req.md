2026-06-10 — Merge-time review failure should not require the
conversation agent to intervene. Today when a Merge Candidate's
merge-time reviewers return `fail`, the Merge Candidate transitions
to `failed` and the lifecycle stops. The conversation agent then has
to draft a new Work Item that cherry-picks the prior candidate
commits and applies fixes for the merge-time findings, then runs
that new Work Item from scratch.
→ Resolved: `4949b04` added `MAX_MERGE_FOLLOWUP_WRITES_PER_INVOCATION`
(2) and refactored `execute_merge` into a loop. On merge-time review
failure with budget remaining, Factory now invokes a follow-up writer
against the candidate workspace with the failed review artifacts as
input, then restarts the rebase + checks + reviews cycle. Budget
exhaustion produces a `needs-user` Merge Candidate state plus a
handoff naming the failed review paths.
