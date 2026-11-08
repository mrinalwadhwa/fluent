Work on this Work Item: {{work_item_id}} - {{work_item_title}}.

{{#if has_task_assignment}}
## Saved Task assignment

Follow the instructions saved on this Writer Task:

{{task_assignment}}

{{/if}}
{{#if is_corrective}}
## Corrective execution context

This is derived corrective Work. There is no brief, behaviors, approach, or plan to read — the block below is the complete, authoritative execution input. You, every reviewer, and the Tester all work from exactly this context. Skip the planning-artifact reading in Phase 1 and treat the deterministic verification as the definition of done.

{{corrective_context}}

{{/if}}
{{#if bootstrap_anything}}
## Phase 0 — Bootstrap

The workspace is missing files Fluent needs. Create them first.

{{/if}}
{{#if bootstrap_tester_yaml}}
### `.fluent/tester.yaml` is missing

Create this file and commit it. It declares which commands the Tester will run after you're done.

Schema:
```yaml
commands:
  - command: <shell command to run>
    test_harness: <identifier: cargo-nextest | cargo-test | shell-harness | pytest | junit>
```

Each entry's `command` is a shell string the Tester runs sequentially in the workspace root. `test_harness` identifies which parser the `extract-tester-results` script uses to normalize the output.

{{/if}}
{{#if bootstrap_extract_script}}
### `.fluent/extract-tester-results` is missing

Create this executable script and commit it.

The script must:

- Take a directory path as its single argument.
- Read `commands.json` from that directory (an array of objects with `command`, `test_harness`, `exit_code`, `duration_ms`, `stdout_log`, `stderr_log`).
- Read the per-command log files (`commands/<n>-stdout.log`, `commands/<n>-stderr.log`).
- Emit a JSON array on stdout, one entry per test:
```json
[{"id": "test_name", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 42, "failure_excerpt": null}]
```
- Every `id` must be globally unique across all commands and harnesses. When a test name recurs across binaries, files, or sections, prefix with the binary, file, or section to disambiguate.
- Preserve native qualification: Rust binary and module, Python module and class, and exact script path. A whole-script result uses `tests/check.sh`; a named shell result uses `tests/check.sh::test_name`. Coverage descriptions and harness display labels do not identify tests.
- Existing native IDs are normalized by Fluent. A producer may also supply `identity`, for example `{"kind":"named-test","path":"tests/check.sh","name":"test_name"}`. Preserve the raw `id` and `status`; a supplied identity must agree with any native qualifiers in the raw ID. The producer cannot supply canonical command occurrence or host acceptance authority.
- `status` must be one of: `pass`, `fail`, `skipped`, `not_run`.
- `failure_excerpt` is a string (at most 500 chars) or null.
- Be executable (`chmod +x`).

{{/if}}
## Phase 1 — Understand the Work Item and the codebase

First, understand what this Work Item asks for:

{{#if has_brief_path}}
- Read Brief at {{brief_path}} — what to change and why.
{{/if}}
{{#if has_behaviors_path}}
- Read Behaviors at {{behaviors_path}} — EARS statements describing observable changes in behavior.
{{/if}}
{{#if has_approach_path}}
- Read Approach at {{approach_path}} — technical direction on how to make the changes.
{{/if}}
{{#if has_plan_path}}
- Read Plan at {{plan_path}} — series of incremental steps; each one makes new changes in behavior observable and verifiable.
{{/if}}
- Read the expertise indexes:
   - {{general_expertise_index}} — general, project-agnostic principles (architecture, testing,
     documentation, tooling). Load individual files on demand in Phase 3, by relevance to each step.
{{#if has_project_expertise_index}}
   - {{project_expertise_index}} — fluent's learned model of THIS project (recorded decisions,
     conventions, patterns, and a codebase overview when present). Read the files it indexes now;
     this is fluent's own accumulated understanding of the project — prefer it over re-deriving.
{{/if}}
{{#if has_prior_reviews}}
- Read each prior correction artifact, including review reports, Tester evidence, or `candidate-gate.json`. The list below is the complete set for this continuation: {{prior_reviews_list}}
{{/if}}
{{#if has_work_item_inputs}}
- Read each preserved Work Item input. These immutable files are authoritative execution inputs: {{work_item_inputs_list}}
{{/if}}

Before implementing, orient to the codebase so your work fits how this project already does things:
- Your writable candidate checkout is {{candidate_checkout_path}}. The source checkout is {{source_checkout_path}}. Make product changes and commits only in the candidate checkout. Keep source product files read-only; declared Task artifacts under the source checkout's `.fluent/work` tree remain writable.
- Skim the existing code around what the plan touches. Match its structure, naming, error
  handling, and idioms.
- Follow the project's stated conventions in its `AGENTS.md` / `CLAUDE.md` (your coding agent
  loads these): code style, naming, commit-message rules, prohibited practices.
Your changes should read as though the project's regular maintainers wrote them.

The Brief, Behaviors, Approach, and Plan files are read-only.

{{#if has_writer_completion_matrix}}
Rows assigned to `host`, `review`, `tester`, or `learner` are read-only context: leave their state pending and their evidence arrays empty. Complete only `writer` rows (or legacy rows with no responsibility) and applicable findings. Fluent verifies host evidence and later stages separately; assignment never authorizes host execution.

## Writer completion matrix

Read {{writer_completion_matrix_path}}. Fluent generated its rows from the approved behaviors and their existing `Test:` or `Untestable:` references, required Plan rows and their existing Verification cells, applicable Approach decisions, structural boundaries, execution guidance, and this round's open review findings.

The row identity, kind, source, requirement, responsibility, and verification references are immutable. Do not add, remove, reorder, or rewrite rows. For each Writer-owned row or applicable finding you satisfy, set `state` to `complete`, list concrete files, symbols, commits, or a verified no-change reason in `implementation-evidence`, and add at least one focused harness-native command with `result: pass` under `verification`. Keep each editable collection as a JSON array, even when it contains one value:

    "state": "complete",
    "implementation-evidence": ["src/module.rs: changed symbol"],
    "verification": [
      {"command": "cargo test focused_case", "result": "pass"}
    ],
    "applicable-constraints": []

For each review-finding row, list applicable Behavior, Approach, or required Plan row IDs in `applicable-constraints`; when none applies, use `["none: <specific reason>"]`. These are the exact allowed IDs for this Attempt:

{{writer_completion_allowed_constraints}}

Fluent validates this matrix before starting ordinary reviewers. Incomplete Writer-owned rows or applicable findings return to Writer work when the no-output, regression, stagnation, and structural pause rules permit continuation. Once Writer work finishes, a pending required host check pauses with the exact candidate, action, and evidence handoff instead of another Writer. Host checks require passing evidence and targeted Tests Review; review, final Tester, and Learner rows remain pending context governed by their separate stage gates. Structural edits or contradictory claims pause for repair. The matrix is advisory evidence for reviewers; it does not replace the project's test selectors or Fluent's final Tester.
{{/if}}

## Phase 2 — Set up progress

progress.md is a `- [ ]` to-do list that persists across rounds. Each item can have nested bullets (two-space indent) for commit hashes, divergences, and notes for the next round. Example:

    - [x] Add streaming API stubs
      - commit a1b2c3d
      - divergence: used `BufRead` instead of `Read`
    - [ ] Implement chunk dispatch
    - [ ] Write integration test

{{#if has_matrix_backed_required_progress}}
Fluent owns the `## Required completion` section in progress.md. It is a read-only projection of the Plan's required rows: use its order to choose work, but do not edit its checkboxes, requirement text, evidence, or nested content. Report completion only in `writer-completion.json`; after validating the matrix, Fluent replaces this section from the immutable Plan contract and points each completed row to its matrix evidence. Record review work under a separate `## Review follow-ups` section outside `## Required completion`.

Each incomplete Writer run must create a candidate commit and must not reduce the number of completed matrix rows. Fluent resumes the same provider session when possible and gives that continuation the unresolved progress, latest candidate change, and current findings. An incomplete no-change run, regressed matrix, or two consecutive Writer continuations against the same obligations, including after review, with no newly completed row pauses for user inspection.
{{/if}}
{{#if has_legacy_required_progress}}
progress.md contains an authoritative `## Required completion` section Fluent materialized from the Plan's required rows. Preserve every stable id and its requirement text exactly. Mark an entry complete only by changing its checkbox and appending concrete `Evidence:`. Keep review work under a separate `## Review follow-ups` section.
{{/if}}
{{#if has_progress_md}}
1. Read progress.md at {{progress_md_path}}.
{{/if}}
{{#if create_progress_from_plan}}
1. Create progress.md at {{progress_md_path}}: identify the steps in plan.md and turn them into a `- [ ]` to-do checklist.
{{/if}}
{{#if create_progress_from_assignment}}
1. Create progress.md at {{progress_md_path}}: turn the saved Task assignment into a `- [ ]` to-do checklist.
{{/if}}
{{#if has_prior_reviews_with_required_progress}}
2. Ensure progress.md has a separate `## Review follow-ups` section outside `## Required completion`.
3. Collect every `- [ ]` finding from the prior review files. Under `## Review follow-ups`, add any missing finding as `- [ ] Address review finding: <title> (from <review-md-path>)`. Do not delete unresolved follow-ups. Address these before remaining required steps.
4. Find the first unchecked review follow-up, or otherwise the first `- [ ]` required item — that is your next step.
{{/if}}
{{#if has_prior_reviews_without_required_progress}}
2. Ensure progress.md has a `## Review follow-ups` section.
3. Collect every `- [ ]` finding from the prior review files. Under `## Review follow-ups`, add any missing finding as `- [ ] Address review finding: <title> (from <review-md-path>)`. Do not delete unresolved follow-ups. Address these before remaining plan steps.
4. Find the first unchecked review follow-up, or otherwise the first `- [ ]` plan or progress item — that is your next step.
{{/if}}
{{#if has_no_prior_reviews}}
2. Find the first `- [ ]` item — that is your next step.
{{/if}}

## Phase 3 — Implement each step

Work through every `- [ ]` item in progress.md, one at a time. For each item:

### A. Understand the step

{{#if has_plan_path}}
1. Refer to sections related to this step in plan.md and behaviors.md.
2. Find approach.md sections related to this step. Note any recommended names, file paths, structural ideas, and constraints.
3. Consult the indexes and read expertise files relevant to this step (architecture, testing, docs, etc.), including any patterns they reference.
4. Identify any tests the plan names for this step. Check the codebase for their presence; record missing tests in the matrix evidence or a mutable progress section before making code changes.
{{else}}
1. Use the saved Task assignment above to identify the requested result and any constraints.
2. Consult the expertise indexes for relevant project conventions and test patterns.
{{/if}}

### B. Implement test-first

Follow test-driven development: write failing tests first, then the code that makes them pass. Refer to testing expertise and patterns on how to write good tests.

1. Write or update tests that capture the step's behavior change. They should fail before the change and pass after.
2. Write failing unit tests before each new function, method, or class. Write failing integration tests for new interactions between modules.
3. Implement the code changes.
4. Run the step's tests and any plan-specified verification. Fix and re-run until everything passes.

Skip TDD only when the step has no new behavior to test. Skip-eligible examples: doc-only changes, mechanical renames, compiler-enforced type updates, dead-code removal, pure refactors covered by existing tests. Hard-to-test code (async, integration surfaces) does not qualify — write tests for it. If the step has new behavior but the Fluent's environment cannot exercise it, see the `Untestable:` rule below. For any skip, record why in progress.md.

### C. Verify and document

1. Review your diff: each change should be intentional and match plan, approach, and behaviors. Remove debug prints, unrelated edits, dead code, or commented-out blocks.
2. Confirm the step's behaviors (from behaviors.md) are now observable.
3. Confirm every approach.md constraint you noted is satisfied (grep, find, or project introspection).
4. If your implementation diverged from plan, approach, or behaviors, record the divergence in the matrix evidence or a mutable progress section — never silently diverge or edit the host-owned required section.
5. Update all documentation the step's change affects — READMEs, CHANGELOGs, API references, inline comments, user guides, etc.

### D. Commit and advance

{{#if is_followup_writer}}
1. When the step requires a correction, make one commit per step. Include everything the step touched — code, tests, `tester.yaml` updates, pattern files, docs — in that commit. Write a message that describes what changed and follows the project's stated commit conventions (`AGENTS.md` / `CLAUDE.md`) — subject length, mood, prohibited trailers, and the like.
   Treat subject length or style, body presence, body-line length or wrapping,
   body formatting, and literal `\n` text as presentation advice: follow it
   when practical, but
   do not create corrective work or block completion solely to satisfy it.
   Continue to obey commit-history, identity, message-encoding, and
   `Co-Authored-By` requirements.
2. If verification proves the previous Writer commit already satisfies the requested correction, preserve the clean candidate without creating a commit. Write {{no_change_declaration_path}} with this exact JSON shape:

   ```json
   {
     "schema_version": 1,
     "reason": "Why no correction was necessary",
     "verification": [
       {"command": "the command you ran", "result": "pass"}
     ]
   }
   ```

   `reason` and every `command` must be non-empty, and `verification` must contain at least one passing entry. Fluent deletes this file before every coder launch, so only evidence produced by this run can authorize no-change completion. Do not use this path when you changed the candidate, when verification did not pass, or when the workspace is dirty.
3. Record completion in the interface described in Phase 2: update the matrix when present and leave its host-owned progress projection unchanged; otherwise mark the progress item with commit or verified no-change evidence.
{{else}}
1. Make one commit per step. Include everything the step touched — code, tests, `tester.yaml` updates, pattern files, docs — in that commit. Write a message that describes what changed and follows the project's stated commit conventions (`AGENTS.md` / `CLAUDE.md`) — subject length, mood, prohibited trailers, and the like.
   Treat subject length or style, body presence, body-line length or wrapping,
   body formatting, and literal `\n` text as presentation advice: follow it
   when practical, but
   do not create corrective work or block completion solely to satisfy it.
   Continue to obey commit-history, identity, message-encoding, and
   `Co-Authored-By` requirements.
2. Record completion in the interface described in Phase 2: update the matrix when present and leave its host-owned progress projection unchanged; otherwise mark the progress item with commit evidence.
{{/if}}
Then move to the next `- [ ]` item.

## Phase 4 — Focused verification

After committing, run `fluent task check-handoff <work-item-id> <attempt-id> <task-id>` from the assigned candidate workspace. It reports each handoff check against the candidate at that moment. A passing preflight does not end the Task or replace Fluent's final post-Writer rerun.

Run the smallest harness-native test selections that give useful feedback for the files and behaviors you changed. Do not run the complete configured suite merely to duplicate Fluent's final Tester gate; Fluent runs that canonical suite once after reviewers pass. Expand your focused selection when failures or the change's risk justify it.

Fluent has assigned this Attempt a private dependency cache through `CARGO_HOME`. Reuse it for focused Cargo commands. The same cache root also provides `UV_CACHE_DIR`, `PIP_CACHE_DIR`, and `npm_config_cache` for package-manager commands. Do not override those variables or read from or write to the operator's global Cargo or package-manager caches. When confirming that admitted dependencies are sufficient, prefer Cargo's `--offline` option.

### Task is done when

{{#if is_followup_writer}}
- Every step is committed or explicitly verified as no-change.
{{else}}
- Every step is committed.
{{/if}}
- Your focused verification succeeds. The final canonical Tester remains Fluent's responsibility after review.
- The workspace has no unstaged, staged, or untracked changes — commit meaningful files; add generated ones to `.gitignore`.

{{#if is_followup_writer}}
A follow-up Write Task with no new commit succeeds only through the verified no-change declaration above. Initial Writers and every Writer without valid fresh evidence must produce a commit.
{{else}}
An initial Write Task with no new commits fails automatically.
{{/if}}

In your final response, include a concise `Verification` section. For every command you ran, report the exact command, pass/fail result, and the behavior or risk it covered. Also state which canonical commands you intentionally left to Fluent's final Tester. This summary is advisory review input; Fluent's final Tester artifact is the authoritative full-suite evidence.

## Rules during step execution

### When you add a new test file

1. Read `.fluent/tester.yaml`. Check whether any declared `command` entry would discover your new tests.
2. If not, add a new `commands:` entry. Model it on existing entries:

   ```yaml
   - command: cargo nextest run --workspace
     test_harness: cargo-nextest
   ```

   `test_harness` must be one of: `cargo-nextest`, `cargo-test`, `shell-harness`, `pytest`, `junit`.
3. Verify the command picks up your new tests. Run it directly. Use the harness' options to list discovered tests if needed.
4. Include the `tester.yaml` update in the same commit as the test file.

### When you invent or observe a reusable pattern

Capture an approach as a pattern when ALL three apply: (1) it would apply to future situations in this codebase, (2) a Writer working from scratch wouldn't naturally arrive at it, (3) you can describe its shape concretely. Examples: test fixtures, code-organization techniques, error-handling conventions.

1. Create the pattern file at `.fluent/expertise/<topic>/patterns/<pattern-name>.md`, where `<topic>` is the narrowest topic that fits (e.g., `tests` for a general testing pattern, or `tests/running-tests` for one specific to test execution). Create intermediate `patterns/` directories if they don't exist. Pattern file structure:
   - Title — what the pattern is
   - Context — when to use it
   - Mechanism — how it works
   - Example — concrete usage
2. Add an index entry under the `## Patterns` section of `.fluent/expertise/<topic>.md`. Use a relative link and a single-line load trigger that says when to read the pattern: `- [pattern-name](patterns/<pattern-name>.md) — read when <trigger>`. Create the `## Patterns` section if it doesn't exist.
3. Include the pattern file and the index update in the same commit as this step's other changes.

### When you can't test a planned behavior

`Untestable:` is a last resort.

1. Consult testing expertise and any patterns it indexes — the obstacle may have a known solution.
2. If you still need to mark `Untestable:`, record it in progress.md as a nested bullet under the step, prefixed `Untestable:`, followed by a three-sentence justification naming a real obstacle the Fluent's execution environment cannot overcome. Example:

   ```
   - [x] Implement chunk dispatch
     - commit b2c3d4e
     - Untestable: streaming latency over a real network — the sandbox blocks egress so latency cannot be measured. The behavior is code-reviewable but the timing observation is unverifiable. No test harness in the sandbox can drive this.
   ```

   Examples of valid obstacles: hardware it can't access, a sandbox restriction it can't lift, a non-deterministic external dependency, an interactive surface that cannot be automated. "Not covered by unit tests," "requires integration setup," or "the execution logic is not covered" are NOT valid — they indicate the work was not done, not that the behavior is untestable.

### When you're stuck

After several attempts on a failing test, stop iterating. Record the obstacle in progress.md.
