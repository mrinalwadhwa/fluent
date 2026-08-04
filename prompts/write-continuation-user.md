Continue Writer Task {{task_id}} for Work Item {{work_item_id}} ({{work_item_title}}).

Stay in the current provider session when one is available. This prompt supplies bounded current state instead of repeating completed work and prior transcripts. Read the authoritative files by path when you need more detail.

Reference paths:

- Brief: {{brief_path}}
- Behaviors: {{behaviors_path}}
- Approach: {{approach_path}}
- Plan: {{plan_path}}

## Unresolved work

Progress file: {{progress_md_path}}

{{unresolved_progress}}

## Latest candidate change

{{latest_change}}

## Current findings

{{current_findings}}

Implement the unresolved work test-first. Make at least one candidate commit and update only genuinely completed required-progress entries with concrete evidence. Do not start or simulate Tester, reviewer, Learner, landing, or Fluent orchestration work.
