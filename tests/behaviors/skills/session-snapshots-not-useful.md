# Scenario: Session snapshots aren't useful enough

## Opening statement
The session snapshots aren't really useful to me yet. I want to be able to actually learn from what happened in a run.

## Hidden context
- The current snapshots capture transcript.jsonl and memory files
- What's actually wanted: a summary of what happened, key decisions made, mistakes, and what the agent learned — not raw data
- Would say "I don't want to read a 10,000 line transcript" if asked what's wrong with the current approach
- Would say "something like a session report — what happened, what went well, what didn't" if asked what they'd prefer
- Hasn't thought about who generates the summary — the agent itself, a reviewer, or a post-processing step
- Would say "the agent should probably write it before exiting" if asked
- Open to the summary being part of the handoff or a separate file
- Cares about this for learning and improving the factory's skills over time

## Evaluation criteria
- Did the agent recognize this as a vague/exploratory request and probe deeper?
- Did it ask what "useful" means to the user?
- Did it ask about the current snapshots and what's missing?
- Did it investigate the existing snapshot code?
- Did it avoid jumping to a solution before understanding the problem?
- Did it surface the question of who generates the summary?
- Is the brief appropriately scoped — not too broad, not too narrow?
- Did it capture the learning/improvement motivation in the Why?
