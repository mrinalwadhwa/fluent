Correct Writer Task {{task_id}} for Work Item {{work_item_id}} ({{work_item_title}}).

Continue in the prior Writer session when Fluent provides it. The generated current context is
the authoritative index for this correction:

- Current context: {{execution_context_path}}
- Progress: {{progress_md_path}}
{{#if has_writer_completion_matrix}}
- Writer completion matrix: {{writer_completion_matrix_path}}
{{/if}}
- Full command output directory: {{artifact_dir}}/commands
{{#if has_corrective_authority}}
- Immutable corrective authority: {{corrective_authority_path}}
{{/if}}

Read the current context first. It contains the exact candidate and base commits, changed files,
every unresolved progress entry, every current review finding, concise passing evidence, and
managed paths to the full historical artifacts. Read a specific historical artifact or a focused
diff only when its summary does not provide enough detail. Do not bulk-print planning files,
expertise, source trees, prior transcripts, or complete test output into the model conversation.

When you run a command that can produce substantial output, save its complete stdout and stderr
under {{artifact_dir}}/commands and inspect only its concise summary or relevant failure excerpt
in the conversation. Keep the complete files for audit.

Apply every current finding test-first while preserving the approved constraints. The original
planning files remain available for a specific question:

- Brief: {{brief_path}}
- Behaviors: {{behaviors_path}}
- Approach: {{approach_path}}
- Plan: {{plan_path}}

Run focused harness-native checks. Reuse Fluent's private `CARGO_HOME`; do not access the
operator's Cargo home or rerun the complete configured suite. Fluent owns the one final Tester.

Create a candidate commit for each real correction and leave the workspace clean. If focused
verification proves no candidate change is needed, write
{{no_change_declaration_path}} with schema version 1, a non-empty `reason`, and a non-empty
`verification` array whose entries each contain a non-empty `command` and `result: "pass"`.
Do not use that declaration after changing the candidate. Update progress and the completion
matrix only with concrete passing evidence.
{{#if has_writer_completion_matrix}}
Preserve every host-owned matrix row and map each completed finding to its applicable behavior or
Approach constraint, or to a specific `none:` reason.
{{/if}}

Finish with a concise verification summary containing each exact focused command, result, and
coverage. Name the complete suite left to Fluent's final Tester.
