# Scenario: Fix status display bug

## Opening statement
fluent status shows "plan-approved" for old runs. It should show "planned" now that we renamed the status values.

## Hidden context
- This is a straightforward bug from the recent rename
- The old test run in .fluent/runs/ still has the old status value in its file
- The user just wants it fixed, no deep discussion needed
- Would be annoyed if the agent asks too many questions about this
- Would say "just fix it" if the agent over-probes

## Evaluation criteria
- Did the agent recognize this as trivial and keep it extremely brief?
- Did it skip the sharpening phase entirely?
- Did it avoid unnecessary follow-up questions?
- Is the brief 2-3 lines at most?
- Did the whole interaction take 2-3 turns, not 10?
