# Scenario: Design approach for automatic format checks

## Opening statement
The formatter check behaviors are defined. Help me design the approach
before we plan the implementation.

## Hidden context
- The behaviors require project-configured checks that can run
  automatically before review or merge.
- The first check type is a formatter check, but the design should leave
  room for more checks later.
- The project may not have any formatter configured; Factory should keep
  working in that case.
- The user is concerned about Factory-specific behavior leaking into
  managed projects.
- The user prefers existing project conventions over Factory inventing a
  universal formatter.
- The main design decision is where check configuration lives and how
  Factory discovers/runs it.
- The user would prefer a small general `checks` concept over a
  hard-coded `format` command if the trade-offs are clear.
- The user does not want the approach to over-specify internal code
  structure.

## Evaluation criteria
- Did the agent read or ask for the brief, behaviors.diff.md, existing
  architecture, and relevant references before evaluating options?
- Did it identify that the core design decision is general checks versus
  a formatter-specific special case?
- Did it present options with trade-offs rather than a single assumed
  solution?
- Did it explain why project-owned configuration avoids forcing every
  managed project to run a Factory-specific formatter?
- Did it avoid manufacturing unnecessary design decisions?
- Did the final artifact use the `# Approach` structure with Expertise,
  Key decisions, Solution outline, Risks, and Open questions?
- Did the Expertise section list relevant references or explicitly say
  no additional expertise applied?
- Did the approach describe direction and boundaries without locking in
  detailed internal implementation?
- Did it include rejected alternatives and the trade-offs of the chosen
  direction?
- Did it set up enough information for plan-execution to break the work
  into steps?
