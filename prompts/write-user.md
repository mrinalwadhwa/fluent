Work on this Work Item: {{work_item_id}} - {{work_item_title}}.

{{#if bootstrap_anything}}
## Phase 0 — Bootstrap

The workspace is missing files Factory needs. Create them first.

{{/if}}
{{#if bootstrap_tester_yaml}}
### `.factory/tester.yaml` is missing

Create this file and commit it. It declares which commands the Tester will run after you're done.

Schema:
```yaml
commands:
  - command: <shell command to run>
    test_harness: <identifier: cargo-nextest | cargo-test | shell-harness>
```

Each entry's `command` is a shell string the Tester runs sequentially in the workspace root. `test_harness` identifies which parser the `extract-tester-results` script uses to normalize the output.

{{/if}}
{{#if bootstrap_extract_script}}
### `.factory/extract-tester-results` is missing

Create this executable script and commit it.

The script must:

- Take a directory path as its single argument.
- Read `commands.json` from that directory (an array of objects with `command`, `test_harness`, `exit_code`, `duration_ms`, `stdout_log`, `stderr_log`).
- Read the per-command log files (`commands/<n>-stdout.log`, `commands/<n>-stderr.log`).
- Emit a JSON array on stdout, one entry per test:
```json
[{"id": "test_name", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 42, "failure_excerpt": null}]
```
- `status` must be one of: `pass`, `fail`, `skipped`, `not_run`.
- `failure_excerpt` is a string (at most 500 chars) or null.
- Be executable (`chmod +x`).

{{/if}}
## Phase 1 — Understand the Work Item

1. Read Brief at {{brief_path}} — what to change and why.
2. Read Behaviors at {{behaviors_path}} — EARS statements describing observable changes in behavior.
3. Read Approach at {{approach_path}} — technical direction on how to make the changes.
4. Read Plan at {{plan_path}} — series of incremental steps; each one makes new changes in behavior observable and verifiable.
5. Read the expertise indexes. Each index is a list of expertise files you can load in Phase 3.
   - {{general_expertise_index}} — architecture, testing, documentation, tooling
{{#if has_project_expertise_index}}
   - {{project_expertise_index}} — workspace-specific decisions, conventions, patterns
{{/if}}
{{#if has_prior_reviews}}
6. Read each prior review file. The list below is the complete set from the most recent prior round: {{prior_reviews_list}}
{{/if}}

The Brief, Behaviors, Approach, and Plan files are read-only.

## Phase 2 — Set up progress

progress.md is a `- [ ]` to-do list that persists across rounds. Each item can have nested bullets (two-space indent) for commit hashes, divergences, and notes for the next round. Example:

    - [x] Add streaming API stubs
      - commit a1b2c3d
      - divergence: used `BufRead` instead of `Read`
    - [ ] Implement chunk dispatch
    - [ ] Write integration test

{{#if has_progress_md}}
1. Read progress.md at {{progress_md_path}}.
{{else}}
1. Create progress.md at {{progress_md_path}}: identify the steps in plan.md and turn them into a `- [ ]` to-do checklist.
{{/if}}
{{#if has_prior_reviews}}
2. Remove any existing `- [ ] Address review finding:` lines from progress.md (leave `- [x] Address review finding:` lines as historical record). Then collect every `- [ ]` finding from the prior review files and add each before the first `- [ ]` item in progress.md as `- [ ] Address review finding: <title> (from <review-md-path>)`. Address these before remaining plan steps.
3. Find the first `- [ ]` item — that is your next step.
{{else}}
2. Find the first `- [ ]` item — that is your next step.
{{/if}}

## Phase 3 — Implement each planned step

Work through every `- [ ]` item in progress.md, one at a time. For each item:

### A. Understand the step

1. Refer to sections related to this step in plan.md and behaviors.md.
2. Find approach.md sections related to this step. Note any recommended names, file paths, structural ideas, and constraints.
3. Consult the indexes and read expertise files relevant to this step (architecture, testing, docs, etc.), including any patterns they reference.
4. Identify any tests the plan names for this step. Check the codebase for their presence; record missing tests in progress.md before making code changes.

### B. Implement test-first

Follow test-driven development: write failing tests first, then the code that makes them pass. Refer to testing expertise and patterns on how to write good tests.

1. Write or update tests that capture the step's behavior change. They should fail before the change and pass after.
2. Write failing unit tests before each new function, method, or class. Write failing integration tests for new interactions between modules.
3. Implement the code changes.
4. Run the step's tests and any plan-specified verification. Fix and re-run until everything passes.

Skip TDD only when the step has no new behavior to test. Skip-eligible examples: doc-only changes, mechanical renames, compiler-enforced type updates, dead-code removal, pure refactors covered by existing tests. Hard-to-test code (async, integration surfaces) does not qualify — write tests for it. If the step has new behavior but the Factory's environment cannot exercise it, see the `Untestable:` rule below. For any skip, record why in progress.md.

### C. Verify and document

1. Review your diff: each change should be intentional and match plan, approach, and behaviors. Remove debug prints, unrelated edits, dead code, or commented-out blocks.
2. Confirm the step's behaviors (from behaviors.md) are now observable.
3. Confirm every approach.md constraint you noted is satisfied (grep, find, or project introspection).
4. If your implementation diverged from plan, approach, or behaviors, record the divergence as a nested bullet under the current step in progress.md — never silently diverge.
5. Update all documentation the step's change affects — READMEs, CHANGELOGs, API references, inline comments, user guides, etc.

### D. Commit and advance

1. Make one commit per step. Include everything the step touched — code, tests, `tester.yaml` updates, pattern files, docs — in that commit. Write a message describing what changed.
2. In progress.md: toggle `- [ ]` to `- [x]` and add a nested bullet below it with the commit hash and anything the next round should know.
3. Move to the next `- [ ]` item.

## Phase 4 — Final verification

Run all the test commands in `.factory/tester.yaml`. Fix and re-run until everything passes.

### Task is done when

- Every step is committed.
- All test commands in `.factory/tester.yaml` succeed.
- The workspace has no unstaged, staged, or untracked changes — commit meaningful files; add generated ones to `.gitignore`.

A Write Task with no new commits fails automatically.

## Rules during step execution

### When you add a new test file

1. Read `.factory/tester.yaml`. Check whether any declared `command` entry would discover your new tests.
2. If not, add a new `commands:` entry. Model it on existing entries:

   ```yaml
   - command: cargo nextest run --workspace
     test_harness: cargo-nextest
   ```

   `test_harness` must be one of: `cargo-nextest`, `cargo-test`, `shell-harness`.
3. Verify the command picks up your new tests. Run it directly. Use the harness' options to list discovered tests if needed.
4. Include the `tester.yaml` update in the same commit as the test file.

### When you invent or observe a reusable pattern

Capture an approach as a pattern when ALL three apply: (1) it would apply to future situations in this codebase, (2) a Writer working from scratch wouldn't naturally arrive at it, (3) you can describe its shape concretely. Examples: test fixtures, code-organization techniques, error-handling conventions.

1. Create the pattern file at `.factory/expertise/<topic>/patterns/<pattern-name>.md`, where `<topic>` is the narrowest topic that fits (e.g., `tests` for a general testing pattern, or `tests/running-tests` for one specific to test execution). Create intermediate `patterns/` directories if they don't exist. Pattern file structure:
   - Title — what the pattern is
   - Context — when to use it
   - Mechanism — how it works
   - Example — concrete usage
2. Add an index entry under the `## Patterns` section of `.factory/expertise/<topic>.md`. Use a relative link and a single-line load trigger that says when to read the pattern: `- [pattern-name](patterns/<pattern-name>.md) — read when <trigger>`. Create the `## Patterns` section if it doesn't exist.
3. Include the pattern file and the index update in the same commit as this step's other changes.

### When you can't test a planned behavior

`Untestable:` is a last resort.

1. Consult testing expertise and any patterns it indexes — the obstacle may have a known solution.
2. If you still need to mark `Untestable:`, record it in progress.md as a nested bullet under the step, prefixed `Untestable:`, followed by a three-sentence justification naming a real obstacle the Factory's execution environment cannot overcome. Example:

   ```
   - [x] Implement chunk dispatch
     - commit b2c3d4e
     - Untestable: streaming latency over a real network — the sandbox blocks egress so latency cannot be measured. The behavior is code-reviewable but the timing observation is unverifiable. No test harness in the sandbox can drive this.
   ```

   Examples of valid obstacles: hardware it can't access, a sandbox restriction it can't lift, a non-deterministic external dependency, an interactive surface that cannot be automated. "Not covered by unit tests," "requires integration setup," or "the execution logic is not covered" are NOT valid — they indicate the work was not done, not that the behavior is untestable.

### When you're stuck

After several attempts on a failing test, stop iterating. Record the obstacle in progress.md.
