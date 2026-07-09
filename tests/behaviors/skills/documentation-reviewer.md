# Scenario: Build a documentation reviewer

## Opening statement
I want to add a documentation reviewer to the fluent.

## Hidden context
- Wants two types: code-aware (sees code + docs) and user-facing
  (sees only what users see). Start with code-aware.
- Motivated by current docs feeling AI-generated and drifting from code
- The fluent builds other software — all software built by the fluent
  should have good docs, not just the fluent itself
- Has a detailed writing skill locally (refine-writing) with AI tells,
  substance checks, fluff scans — this needs to be captured into the
  fluent since it won't be available at runtime
- Thinks good documentation and good architecture overlap significantly
- Would say "scoped to the current run usually, full codebase sometimes"
  if asked about scope
- Would say "findings, not rewrites" if asked about what the reviewer
  produces
- Hasn't decided on review artifact format or how it integrates into
  the session loop
- Would say "both humans and coding agents" if asked about audience
- Cares about documentation structure and completeness beyond just
  prose quality — diagrams, C4 model, etc.

## Evaluation criteria
- Did the agent ask what "documentation reviewer" means?
- Did it probe the two-reviewer distinction?
- Did it ask about scope (per-run vs full codebase)?
- Did it ask about what the reviewer produces?
- Did it read existing documentation and architecture?
- Did it surface the runtime availability issue (local skills not
  available on Fargate)?
- Is the brief the right length for a moderately complex feature?
- Did the conversation handle the user's evolving thinking (two
  reviewers → start with one, writing skill → needs to be captured)?
