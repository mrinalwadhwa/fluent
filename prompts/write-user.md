Execute this Factory write Task.

Work Item: {{work_item_id}} - {{work_item_title}}
Attempt: {{attempt_id}}
Task: {{task_id}}
Role: {{role}}

Completion contract:
- Commit all Task output in the writable workspace before marking the Task complete.
- Leave the writable workspace clean: no unstaged, staged, or untracked Task changes.
- If no code, documentation, skill, behavior, or other repository change is needed, do not mark the Task complete; under the current write Task executor contract, no committed Task output makes the Task fail.

Author preflight:
- Before editing, identify the likely touched surfaces: behavior statements, user-facing docs, tests, skills/expertise, and verification commands.
- When changing a user-facing command, behavior, skill, or documentation surface, update the applicable behavior contract, docs, tests, and verification notes in this first pass.
- If this Task is intentionally code-only or docs-only, record why the other related artifacts do not apply instead of adding churn.
{{#if has_input_artifacts}}
- This Task has input artifacts. Read the review input artifacts first, address the concrete findings, and check whether each finding reveals a missing first-pass preflight item.
{{/if}}

{{#if task_instructions}}
Task instructions:
{{task_instructions}}

{{/if}}
Input artifacts:
{{input_artifacts_list}}

progress_md_path: {{progress_md_path}}

Current Task model:
{{task_json}}
{{#if bootstrap_tester_yaml}}

## Bootstrap: .factory/tester.yaml

The candidate workspace is missing `.factory/tester.yaml`. Author this file and commit it alongside your Work Item changes.

The file declares which commands the Tester subcommand runs. Schema:
```yaml
commands:
  - command: <shell command to run>
    test_harness: <identifier: cargo-nextest | cargo-test | shell-harness>
```

Each entry's `command` is a shell string Tester runs sequentially in the workspace root. `test_harness` identifies which parser the `extract-tester-results` script uses to normalize the output.
{{/if}}
{{#if bootstrap_extract_script}}

## Bootstrap: .factory/extract-tester-results

The candidate workspace is missing `.factory/extract-tester-results`. Author this executable script and commit it alongside your Work Item changes.

Contract:
- Receives the artifact directory as its single argument.
- Reads `commands.json` from that directory (array of objects with `command`, `test_harness`, `exit_code`, `duration_ms`, `stdout_log`, `stderr_log`).
- Reads the per-command log files (`commands/<n>-stdout.log`, `commands/<n>-stderr.log`).
- Emits a JSON array on stdout, one entry per test:
```json
[{"id": "test_name", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 42, "failure_excerpt": null}]
```
- `status` must be one of: `pass`, `fail`, `skipped`, `not_run`.
- `failure_excerpt` is a string (at most 500 chars) or null.
- Make the file executable (`chmod +x`).
{{/if}}
