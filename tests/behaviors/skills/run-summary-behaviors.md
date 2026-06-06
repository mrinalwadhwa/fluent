# Scenario: Define behaviors for run summaries

## Opening statement
We already captured a brief for showing a concise run summary in the
dashboard. Help me define the behaviors before we design it.

## Hidden context
- The brief says completed runs should have a summary tab in the
  dashboard so the user can understand what happened without reading the
  whole transcript.
- The user cares about completed and failed runs first; active runs can
  continue showing the live transcript by default.
- The term "summary" means a durable report generated from run
  artifacts, not a live model-generated chat response.
- The user expects the dashboard to prefer the report when a completed
  run has one, but still allow switching back to author and reviewer
  transcripts.
- If no report exists, the dashboard should keep the current behavior
  and show the author transcript.
- The report should include run status, sessions, reviewer verdicts,
  changed files or commits if available, and open questions or handoff
  notes.
- The user does not want implementation details like which Rust module
  or parser should be used in the behavior statements.
- Would say "call it report, not summary, if that matches the existing
  code" if the agent notices existing vocabulary.

## Evaluation criteria
- Did the agent read or ask for the brief and existing behaviors before
  elaborating?
- Did it establish vocabulary around "summary", "report", "run
  artifacts", and "dashboard tab"?
- Did it map actors, events, and states before drafting EARS statements?
- Did it work area by area rather than dumping a full behavior document
  immediately?
- Did the behavior statements stay observable and avoid implementation
  choices?
- Did it cover the fallback when no report exists?
- Did it cover keeping transcript tabs accessible after defaulting to a
  report?
- Did it identify at least one open question suitable for design rather
  than silently deciding it?
