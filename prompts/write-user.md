Execute this Factory write Task.

Work Item: {{work_item_id}} - {{work_item_title}}
Attempt: {{attempt_id}}
Task: {{task_id}}
Role: {{role}}

{{#if brief_path}}
Brief: {{brief_path}}
{{/if}}
{{#if behaviors_path}}
Behaviors: {{behaviors_path}}
{{/if}}
{{#if approach_path}}
Approach: {{approach_path}}
{{/if}}
{{#if plan_path}}
Plan: {{plan_path}}
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
