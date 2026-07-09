# Scenario: Define behaviors for automatic format checks

## Opening statement
We already have a brief for project checks. Help me define the behaviors
for a formatter check that runs automatically before merge.

## Hidden context
- The brief says Fluent should support project-defined checks, starting
  with a formatter check.
- The user wants this to work for any project Fluent manages, not just
  Fluent itself.
- "Formatter check" means a configured command that may modify files and
  must leave the run worktree formatted before review or merge.
- The user wants opt-in automation: once the project configures a check,
  Fluent should run it without stopping for manual input.
- If the check changes files, the author work should include those
  changes and review should see the formatted diff.
- If the check fails after running, Fluent should capture the command
  output and let the author or a follow-up run fix it.
- The user does not want behavior statements to name a specific config
  file, Rust module, or formatter implementation.
- Would say "call them checks, not hooks" if asked about vocabulary.

## Evaluation criteria
- Did the agent read or ask for the brief and existing behaviors before
  elaborating?
- Did it establish vocabulary around "check", "formatter check",
  "configured command", and "manual input"?
- Did it map actors, events, and states before drafting EARS statements?
- Did it work area by area rather than dumping a full behavior document
  immediately?
- Did the final artifact use the `# Behaviors (diff)` structure with
  Vocabulary, behavior areas, and Open questions?
- Did the behavior statements use observable EARS-style wording?
- Did it cover opt-in automatic execution without manual intervention?
- Did it cover both successful formatting changes and formatter command
  failure?
- Did it avoid implementation choices such as exact config schema,
  Rust module names, or a specific formatter?
- Did it identify at least one open question suitable for
  design-approach rather than silently deciding it?
