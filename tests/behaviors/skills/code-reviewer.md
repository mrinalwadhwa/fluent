# Scenario: Add a code reviewer

## Opening statement
I want to add a code reviewer to the factory.

## Hidden context
- Has a general sense that the author's code should be reviewed before a run completes
- Hasn't thought through the mechanics — when does review happen, what triggers it, what happens on failure
- Would say "after the author finishes implementing" if asked about timing
- Would say "quality, patterns, whether it fits the architecture" if asked what the reviewer should look at
- Would say "it should fail the run and explain why, then the author fixes it" if asked about the review verdict
- Hasn't considered whether the reviewer sees the full codebase or just the diff
- Would say "just the diff, plus enough context to understand it" if asked
- Hasn't considered whether this is a separate agent session or the same session
- Would say "probably separate — I don't want the author reviewing its own work" if asked
- Doesn't have a strong opinion on whether review is blocking or advisory
- Would lean toward "blocking — if the review fails, the author has to fix it" if pressed

## Evaluation criteria
- Did the agent recognize this as partially clear (what is clear, how is not)?
- Did it probe the mechanics — when, what, and what happens on failure?
- Did it ask about scope of what the reviewer sees?
- Did it ask about separation from the author?
- Did it read the existing review-related code and architecture?
- Did it avoid designing the solution (that's design-approach's job)?
- Did it surface the blocking vs advisory question?
- Is the brief the right length for a moderately complex feature?
