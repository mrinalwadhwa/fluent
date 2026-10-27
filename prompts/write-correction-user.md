Correct Writer Task {{task_id}} for Work Item {{work_item_id}} ({{work_item_title}}).

Candidate checkout: {{candidate_checkout_path}}
Source checkout: {{source_checkout_path}}
Commit product changes only in the candidate checkout. Keep source product files read-only; declared Task artifacts under the source checkout's `.fluent/work` tree remain writable.

After committing, run `fluent task check-handoff {{work_item_id}} {{attempt_id}} {{task_id}}` from the candidate checkout. Its results describe the candidate at that moment; Fluent reruns the handoff after this Task ends.

Continue in the prior Writer session when Fluent provides it. The generated current context is
the authoritative index for this correction:

- Current context: {{execution_context_path}}
- Progress: {{progress_md_path}}
{{#if has_writer_completion_matrix}}
Rows assigned to `host`, `review`, `tester`, or `learner` are read-only context: leave their state pending and their evidence arrays empty. Complete only `writer` rows (or legacy rows with no responsibility) and applicable findings. Fluent verifies host evidence and later stages separately; assignment never authorizes host execution.

- Writer completion matrix: {{writer_completion_matrix_path}}
{{/if}}
- Full command output directory: {{artifact_dir}}/commands
{{#if has_corrective_authority}}
- Immutable corrective authority: {{corrective_authority_path}}
{{/if}}

Current correction artifacts (complete, untruncated paths):
{{correction_artifacts_list}}

{{#if has_candidate_metadata_rewrite_authority}}
## Candidate metadata-rewrite authority

This Writer was created by an audited Fluent recovery for the exact rejected
candidate. Read the complete authority record before changing Git history:

- Authority: {{candidate_metadata_rewrite_authority_path}}

It authorizes only a tree-identical commit-message rewrite after the protected
base named in that record. Preserve the candidate tree and all protected
ancestry. Do not use this authority to change product files, tests, or planning
content. Fluent will verify the replacement candidate and rerun its ordinary
candidate gate, Tester, reviewers, and Learner.
{{/if}}

{{#if is_tester_harness_repair}}
## Tester-harness repair authority

This is a narrowly scoped project Tester repair. The operator's recorded decision below is the
complete correction authority. Failed Tester commands are diagnostic evidence, not permission to
repair product code or product tests.

{{tester_harness_repair_instructions}}
{{/if}}

{{#if has_human_resolution}}
## Human resolution

The operator made this decision for the exact candidate and failed-review bytes that authorized
this correction. Treat it as authoritative within the approved Brief, Behaviors, Approach, and
Plan:

{{human_resolution}}
{{/if}}

Read the current context first. It contains the exact candidate and base commits, changed files,
every unresolved progress entry, every current finding, concise passing evidence, and
managed paths to the full historical artifacts. For review findings, read a specific historical
artifact or a focused diff only when its summary does not provide enough detail. Do not bulk-print planning files,
expertise, source trees, prior transcripts, or complete test output into the model conversation.

Correction lineage is host-owned:

- Mode: `{{correction_lineage_mode}}`
- Prior candidate commit: `{{prior_candidate_commit}}`

{{#if correction_lineage_append_only}}
Keep the prior candidate commit in the candidate's history. Create a new correction commit for
every content fix, including reverting unwanted files or changes. Do not rebase, reset, amend,
drop, or replace the prior candidate commit. Before finishing, verify that
`{{prior_candidate_commit}}` is an ancestor of `HEAD`.
{{/if}}
{{#if correction_lineage_metadata_rewrite}}
Fluent explicitly authorizes a tree-identical metadata rewrite of
`{{prior_candidate_commit}}`. Change commit metadata only; the resulting candidate tree must
remain exactly identical to that prior candidate's tree. Do not change file content as part of
this rewrite. Commit `{{protected_base_commit}}` is the protected base: do not amend, rebase,
drop, or replace it, and verify that it remains an ancestor of `HEAD`. Rewrite only commits after
that protected base. Fluent will verify the tree identity and protected-base ancestry afterward.
{{/if}}

{{#if has_recovered_protected_base}}
This Attempt contains an audited host ancestry repair. Commit
`{{recovered_protected_base}}` and all history before it are immutable at this
frontier. Do not rewrite their metadata even if a review finding requests it;
record that the host boundary supersedes that part of the finding and address
only product content and commits after the protected base.
{{/if}}

{{#if has_failed_tester_inspection}}
Failed final-Tester evidence is attached. Read every artifact below before deciding that no
candidate change is needed:
{{failed_tester_inspection_list}}

Inspect the candidate's `.fluent/tester.yaml` and focused path/configuration variants alongside
the failed command logs. A command, cache path, package path, or harness configuration committed
in `.fluent/tester.yaml` is candidate-owned; do not classify its failure as external merely
because the command ran in a sandbox.
{{/if}}

When you run a command that can produce substantial output, save its complete stdout and stderr
under {{artifact_dir}}/commands and inspect only its concise summary or relevant failure excerpt
in the conversation. Keep the complete files for audit.

Apply every current finding test-first while preserving the approved constraints. The original
planning files remain available for a specific question:

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

Run focused harness-native checks. Reuse Fluent's private `CARGO_HOME`; do not access the
operator's Cargo home or rerun the complete configured suite. Fluent owns the one final Tester.

Create a candidate commit for each real correction and leave the workspace clean. If focused
verification proves no candidate change is needed, write
{{no_change_declaration_path}} with this exact shape:

```json
{{#if has_failed_tester_inspection}}
{
  "schema_version": 1,
  "reason": "Why no correction was necessary after inspecting the failed Tester evidence",
  "verification": [
    {"command": "the focused command you ran", "result": "pass"}
  ],
  "inspected_artifacts": [
    {
      "path": "one exact project-relative path listed above",
      "digest": "sha256:<digest you computed from that file's bytes>"
    }
  ]
}
{{else}}
{
  "schema_version": 1,
  "reason": "Why no correction was necessary",
  "verification": [
    {"command": "the command you ran", "result": "pass"}
  ]
}
{{/if}}
```

Keep `reason` and every `command` non-empty, and include at least one passing
`verification` entry.
{{#if has_failed_tester_inspection}}
Include exactly one `inspected_artifacts` entry for every failed Tester evidence path listed
above, with no extras or duplicates. Compute each digest from the artifact itself; Fluent rejects
missing, stale, or mismatched evidence before it launches reviewers or reruns the final Tester.
{{/if}}
Do not use that declaration after changing the candidate.
{{#if has_writer_completion_matrix}}
Update mutable progress sections and the completion matrix only with concrete passing evidence.
Preserve every host-owned matrix row. Keep `implementation-evidence`, `verification`, and
`applicable-constraints` as JSON arrays, even when one value exists. Map each completed finding to
its applicable Behavior, Approach, or required Plan constraint, or to `["none: <specific reason>"]`.
These are the exact allowed IDs for this Attempt:

{{writer_completion_allowed_constraints}}

Do not edit the host-owned `## Required completion` projection in progress.md.
{{else}}
Update progress only with concrete passing evidence.
{{/if}}

Finish with a concise verification summary containing each exact focused command, result, and
coverage. Name the complete suite left to Fluent's final Tester.
