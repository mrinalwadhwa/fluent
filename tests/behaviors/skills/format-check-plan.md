# Scenario: Plan execution for automatic format checks

## Opening statement
The approach for project checks is approved. Help me plan the execution.

## Hidden context
- The approach chose a general project-configured checks concept with an
  initial formatter use case.
- The implementation should start with a thin end-to-end slice: a test
  project can define a formatter command, Factory runs it, and changed
  files remain in the run worktree.
- The user wants the run to complete autonomously once started, without
  manual intervention for expected formatter changes.
- This is probably a single run, not parallel child runs, because config
  parsing, command execution, and landing behavior need to fit together.
- The user wants verification points at each step, not a vague to-do
  list.
- Optional scope: automatic review rerun after a formatter changes files
  could be deferred only if needed, but the user previously questioned
  delaying that too much.
- Risks include shell command safety, dirty worktrees, and ensuring a
  failed formatter does not leave Factory stuck waiting for human input.

## Evaluation criteria
- Did the agent read or ask for brief, behaviors.diff.md, approach.md,
  and architecture references before planning?
- Did it assess whether this should be a single run or decomposed into
  child runs?
- Did it propose a walking skeleton first that proves the configured
  check can run end-to-end?
- Did the plan steps describe observable states reached rather than only
  activities?
- Did every step include verification?
- Did every behavior area have a home in a step or an explicit TBD?
- Did it classify required versus optional work and discuss scope trades?
- Did it surface risks around command execution, dirty worktrees, and
  failed checks?
- Did the final artifact use the `# Plan` structure with Steps, Scope
  trades, and Risks?
- Did it avoid redesigning the approach during planning?
