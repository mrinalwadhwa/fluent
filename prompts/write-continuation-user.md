Continue Writer Task {{task_id}} for Work Item {{work_item_id}} ({{work_item_title}}).

Candidate checkout: {{candidate_checkout_path}}
Source checkout: {{source_checkout_path}}
Commit product changes only in the candidate checkout. Keep source product files read-only; declared Task artifacts under the source checkout's `.fluent/work` tree remain writable.

After committing, run `fluent task check-handoff {{work_item_id}} {{attempt_id}} {{task_id}}` from the candidate checkout. Its results describe the candidate at that moment; Fluent reruns the handoff after this Task ends.

Stay in the current provider session when one is available. This prompt supplies bounded current state instead of repeating completed work and prior transcripts. Read the authoritative files by path when you need more detail.

Reference paths:

{{#if brief_path}}
- Brief: {{brief_path}}
{{/if}}
{{#if behaviors_path}}
- Behaviors: {{behaviors_path}}
{{/if}}
{{#if approach_path}}
- Approach: {{approach_path}}
{{/if}}
{{#if plan_path}}
- Plan: {{plan_path}}
{{/if}}
- Generated current context: {{execution_context_path}}
{{#if has_writer_completion_matrix}}
Rows assigned to `host`, `review`, `tester`, or `learner` are read-only context: leave their state pending and their evidence arrays empty. Complete only `writer` rows (or legacy rows with no responsibility) and applicable findings. Fluent verifies host evidence and later stages separately; assignment never authorizes host execution.

- Writer completion matrix: {{writer_completion_matrix_path}}
{{/if}}

Read the generated context first. It contains the exact candidate/base commits,
changed files, unresolved steps, deduplicated current findings, passing Tester
evidence, executed commands, and paths to historical artifacts. Inspect those
paths or run `git show HEAD` only when you need the underlying detail.

## Unresolved work

Progress file: {{progress_md_path}}

{{unresolved_progress}}

Implement the unresolved work test-first. Use the test harness's native selectors to run focused checks for the correction; do not rerun the complete configured suite merely to duplicate Fluent's final Tester gate. Commit source changes. An unchanged candidate requires fresh no-change evidence under the completion contract; an incomplete Writer run still requires a new candidate commit.
{{#if has_writer_completion_matrix}}
Record completion and concrete evidence in the Writer completion matrix; do not edit the host-owned `## Required completion` projection in progress.md.
{{else}}
Update only genuinely completed required-progress entries with concrete evidence.
{{/if}}
Do not start or simulate Tester, reviewer, Learner, landing, or Fluent orchestration work.

{{#if correction_lineage_append_only}}
Preserve candidate {{prior_candidate_commit}} and all of its ancestors. Append
new commits for source changes. Historical gate reports and literal backslash-n
text do not authorize rewriting this history.
{{/if}}
{{#if correction_lineage_metadata_rewrite}}
The host validated a tree-identical metadata rewrite for candidate
{{prior_candidate_commit}}. Preserve protected base {{protected_base_commit}}
and all of its ancestors. Repair only the authorized range without changing the
candidate tree. Fluent rechecks this exact authority and the complete range.
{{/if}}

{{#if has_writer_completion_matrix}}
Update the Writer completion matrix as you work. Preserve every host-owned row field exactly. Mark a Writer-owned row or applicable finding complete only with concrete implementation evidence and at least one passing focused command. Keep `implementation-evidence`, `verification`, and `applicable-constraints` as JSON arrays, even when one value exists. A review-finding row must also name an applicable Behavior, Approach, or required Plan row ID, or `["none: <specific reason>"]` when no approved constraint applies. These are the exact allowed IDs for this Attempt:

{{writer_completion_allowed_constraints}}

Fluent checks the matrix before reviewers and the final Tester.
{{/if}}

In your final response, include a concise `Verification` section with each exact command, its pass/fail result, and what it covered. State which canonical commands you intentionally left to Fluent's final Tester. This summary is advisory review input; Fluent's final Tester artifact is the authoritative full-suite evidence.
