Continue Writer Task {{task_id}} for Work Item {{work_item_id}} ({{work_item_title}}).

Stay in the current provider session when one is available. This prompt supplies bounded current state instead of repeating completed work and prior transcripts. Read the authoritative files by path when you need more detail.

Reference paths:

- Brief: {{brief_path}}
- Behaviors: {{behaviors_path}}
- Approach: {{approach_path}}
- Plan: {{plan_path}}
- Generated current context: {{execution_context_path}}
{{#if has_writer_completion_matrix}}
- Writer completion matrix: {{writer_completion_matrix_path}}
{{/if}}

Read the generated context first. It contains the exact candidate/base commits,
changed files, unresolved steps, deduplicated current findings, passing Tester
evidence, executed commands, and paths to historical artifacts. Inspect those
paths or run `git show HEAD` only when you need the underlying detail.

## Unresolved work

Progress file: {{progress_md_path}}

{{unresolved_progress}}

Implement the unresolved work test-first. Use the test harness's native selectors to run focused checks for the correction; do not rerun the complete configured suite merely to duplicate Fluent's final Tester gate. Make at least one candidate commit and update only genuinely completed required-progress entries with concrete evidence. Do not start or simulate Tester, reviewer, Learner, landing, or Fluent orchestration work.

{{#if has_writer_completion_matrix}}
Update the Writer completion matrix as you work. Preserve every host-owned row field exactly. Mark a row complete only with concrete implementation evidence and at least one passing focused command. A review-finding row must also name the applicable behavior or approach row ID, or `none: <specific reason>` when no approved constraint applies. Fluent checks the matrix before reviewers and the final Tester.
{{/if}}

In your final response, include a concise `Verification` section with each exact command, its pass/fail result, and what it covered. State which canonical commands you intentionally left to Fluent's final Tester. This summary is advisory review input; Fluent's final Tester artifact is the authoritative full-suite evidence.
