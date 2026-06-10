# Scenario: Plan execution for automatic format checks

## Opening statement
The approach for project checks is approved. Help me plan the execution.

## Hidden context
- The approach chose a general project-configured checks concept with an
  initial formatter use case.
- The implementation should start with a thin end-to-end slice: a test
  project can define a formatter command, Factory runs it, and changed
  files remain in the Task workspace.
- The user wants the Task to complete autonomously once started, without
  manual intervention for expected formatter changes.
- This probably fits one Work Item with one Attempt and one write Task,
  not peer Work Items, because config parsing, command execution, and
  landing behavior need to fit together. Later Tasks can be noted as
  likely follow-up planning, but they are not executable dependencies.
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
- Did it assess whether this should be one Work Item with one Attempt
  and one write Task, likely follow-up Task notes, or peer Work Items?
- Did it propose a walking skeleton first that proves the configured
  check can run end-to-end?
- Did the plan steps describe observable states reached rather than only
  activities?
- Did every step include verification?
- Did every behavior area have a home in a step or an explicit TBD?
- Did it classify required versus optional work and discuss scope trades?
- Did it surface risks around command execution, dirty worktrees, and
  failed checks?
- Did the final artifact use the Work Item planning shape with `# Plan`,
  `Work Item`, `Attempt`, a Steps table that includes a `Work unit`
  column, `Dependencies and sync points`, Scope trades, and Risks?
- Did it avoid presenting separate Work-model Tasks with explicit
  dependencies as the default executable parallel structure?
- Did it avoid redesigning the approach during planning?
