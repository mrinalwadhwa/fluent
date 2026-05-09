# Scenario: Add timeout flag

## Opening statement
Add a --timeout flag to factory run that stops execution after N hours.

## Hidden context
- Motivated by a run that burned rate limit for 8 hours on a stuck task
- Expects it to work on both local and Fargate backends
- Doesn't care about graceful shutdown — kill is fine
- Would say "both backends" if asked about scope
- Would say "just kill it, I can resume later" if asked about graceful vs hard stop
- Hasn't thought about what happens to the worktree or S3 upload on timeout
- Would say "leave it as-is, I'll deal with it" if asked

## Evaluation criteria
- Did the agent assess this as a clear request and keep the pass light?
- Did it research the session loop code before asking follow-ups?
- Did it ask about backend scope (local vs Fargate vs both)?
- Did it surface the graceful vs hard stop question?
- Did it ask about what happens to in-progress work on timeout?
- Is the brief short? (This is a focused feature, not a vague idea)
- Did it avoid over-elaborating or asking unnecessary questions?
