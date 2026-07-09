# Scenario: Make the fluent work better for multiple runs

## Opening statement
I want the fluent to handle multiple runs at the same time better.

## Hidden context
- This is deliberately vague — the user hasn't fully thought it through
- The real trigger: they had two runs going and fluent status was confusing — hard to tell which was which
- Also noticed that parallel Fargate runs share the same rate limit, which surprised them
- Would say "I had two runs going and it was confusing" if asked why
- Would say "status was messy, couldn't tell which run was doing what" if asked what was confusing
- Would mention rate limits if asked "anything else?"
- Hasn't thought about whether parallel runs should share a worktree or have separate ones (they already have separate ones, but the user might not realize)
- Would say "oh, they already get separate worktrees? that's fine then" if the agent mentions the current behavior
- The real ask might shrink to just "improve fluent status display for multiple runs" once probed
- Open to the scope narrowing during the conversation

## Evaluation criteria
- Did the agent recognize this as vague and dig deeper?
- Did it ask what triggered the request?
- Did it discover the real issue (status display, not fundamental architecture)?
- Did it read the fluent status code?
- Did it mention that parallel runs already get separate worktrees?
- Did the scope narrow naturally through conversation?
- Did it use any sharpening tools (5 whys, WYSIATI)?
- Is the final brief focused on the actual problem, not the vague opening?
