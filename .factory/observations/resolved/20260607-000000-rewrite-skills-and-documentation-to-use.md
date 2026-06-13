2026-06-07 — Rewrite skills and documentation to use the new Work-model
vocabulary. Briefs, behaviors, approaches, and plans should attach to
Work Items and Attempts; execution should happen through Tasks; landing
should happen through Merge Candidates. Legacy `.factory/runs` guidance
should remain only as a temporary bridge until the new execution path
works end to end.
→ Resolved: 8ebf4b2 (the build workflow skill now teaches Work Item →
Attempt → Task → Workspace → Merge Candidate as the target lifecycle,
related planning/review skills and architecture/behavior docs use the
new vocabulary, and focused behavior tests cover the Work guidance and
command reference)
