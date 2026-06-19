Execute this Factory review Task.

Work Item: {{work_item_id}} - {{work_item_title}}
Attempt: {{attempt_id}}
Task: {{task_id}}
Role: {{role}}

{{#if task_instructions}}
Task instructions:
{{task_instructions}}

{{/if}}
{{#if input_artifacts_block}}
{{input_artifacts_block}}

{{/if}}
{{#if review_only}}
Readable source checkout:
{{read_paths}}

Review context:
- Source checkout: {{candidate_workspace_id}} ({{candidate_workspace_path}})
- Source ref: {{source_branch}}
- Source commit: {{candidate_commit}}
{{else}}
Readable candidate workspaces:
{{read_paths}}

Review context:
- Candidate workspace: {{candidate_workspace_id}} ({{candidate_workspace_path}})
- Source branch: {{source_branch}}
- Candidate commit: {{candidate_commit}}
- Review diff: {{review_diff_command}}
{{/if}}
{{#if behavior_review_input}}
{{behavior_review_input}}
{{/if}}

Work review artifact path:
{{artifact_path}}
Write the review artifact to exactly this filesystem path:
{{review_path}}
Your reviewer artifact directory is:
{{artifact_dir}}

Build cache and writable outputs:
- You may READ the candidate workspace's existing build outputs (binaries, compiled artifacts, installed dependencies) freely. The writer produced them as part of completing the write task.
- You may NOT write to the candidate workspace, including its build outputs. Concurrent reviewers cannot safely share a build cache.
- Factory has pre-populated your reviewer artifact directory at {{artifact_dir}} with copies of the writer's build outputs for warm-start incremental builds. When you need to build new outputs the writer didn't produce, redirect them there.
- For Cargo: CARGO_TARGET_DIR="{{artifact_dir}}/target" cargo build (or cargo test). If the writer already built the binary you need, invoke it directly from the candidate workspace instead of recompiling.

The Task completes when that artifact exists. The artifact may contain Verdict: pass, Verdict: fail, or Verdict: uncertain; do not edit {{edit_target}}.

Current Task model:
{{task_json}}
