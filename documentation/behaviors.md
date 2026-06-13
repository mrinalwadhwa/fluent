# Behaviors

Observable behaviors of the factory system. Each statement describes what
the system does, not how. EARS format.

RunBehaviorTests: cargo nextest run --test binary --message-format libtest-json
RunBehaviorTests: cargo test --lib --message-format json
RunBehaviorTests: for test in tests/behaviors/operations/*.sh; do bash "$test"; done

## Test harnesses

| Harness | Runs | Usage |
|---|---|---|
| `tests/test-skill` | Skill conversation simulations | `tests/test-skill <scenario> <skill> [--judge]` |
| `tests/test-run` | Operational assertions | `tests/test-run` |

WHEN `tests/test-skill` completes a skill conversation simulation,
THE HARNESS SHALL print the run directory and list `transcript.md` as
the full conversation artifact. The harness SHALL list `brief.md` only
when it extracted a captured artifact from the skill agent response, and
SHALL list `verdict.md` only when the judge wrote scoring.
Test: tests/behaviors/operations/test-skill-harness-artifacts.sh

---

## Version reporting

WHEN `factory version` is invoked,
THE SYSTEM SHALL print the Factory package version and build commit to
stdout and exit successfully without requiring a Factory project.
Test: tests/binary.rs (version_prints_package_version_and_commit)
Test: tests/behaviors/operations/test-version.sh

## Observations management

WHEN `factory observations add "<content>"` is invoked,
THE SYSTEM SHALL write a new observation file at
`.factory/observations/<id>.md` where `<id>` follows the format
`YYYYMMDD-HHMMSS-<short-title>` derived from the current timestamp
and a sanitized kebab form of the content's first line, and SHALL
print the generated `<id>` on stdout.
Test: tests/binary.rs (observations_add_with_inline_content)
Test: src/observations.rs (generate_id_includes_timestamp_and_slug)

WHEN `factory observations add` is invoked without an inline content
argument,
THE SYSTEM SHALL read the observation body from stdin and SHALL fail
with a clear error if stdin is empty.
Test: tests/binary.rs (observations_add_from_stdin)
Test: tests/binary.rs (observations_add_empty_stdin_errors)

WHEN two `factory observations add` invocations would generate the
same `<id>` (same second, same first-line title),
THE SYSTEM SHALL suffix the second with a counter
(`YYYYMMDD-HHMMSS-<short-title>-2`) so the resulting filenames are
unique.
Test: src/observations.rs (resolve_collision_sequential_suffixes)
Test: src/observations.rs (migrate_collision_suffixes)

WHEN `factory observations resolve <id> "<resolution>"` is invoked
and `<id>` matches exactly one file under `.factory/observations/`,
THE SYSTEM SHALL append the resolution context to the file
(preserving the existing observation content) and move the file to
`.factory/observations/resolved/<id>.md`.
Test: tests/binary.rs (observations_resolve_inline)

WHEN `factory observations resolve <id>` is invoked without an inline
resolution argument,
THE SYSTEM SHALL read the resolution body from stdin and SHALL fail
with a clear error if stdin is empty.
Untestable: Requires interactive stdin pipe not supported by current binary test harness

IF `<id>` matches zero open observation files when resolving,
THEN THE SYSTEM SHALL exit non-zero with an error naming the missing
id.
Test: tests/binary.rs (observations_resolve_unknown_id_errors)

IF `<id>` is supplied as a unique prefix of exactly one open
observation id,
THEN THE SYSTEM SHALL expand it to the full id for `resolve` and
`show`.
Test: tests/binary.rs (observations_resolve_prefix_unique_match)
Test: src/observations.rs (expand_prefix_unique_match)

IF `<id>` is supplied as a prefix that matches multiple ids,
THEN THE SYSTEM SHALL exit non-zero, list the matches, and ask the
user to disambiguate.
Test: tests/binary.rs (observations_resolve_prefix_ambiguous_errors)
Test: src/observations.rs (expand_prefix_ambiguous)

WHEN `factory observations list` is invoked,
THE SYSTEM SHALL print one line per open observation under
`.factory/observations/`, ordered by id ascending (chronological), in
the format `<id>  <first line of body>`.
Test: tests/binary.rs (observations_list_orders_chronologically)

WHEN `factory observations show <id>` is invoked,
THE SYSTEM SHALL print the body of the observation at
`.factory/observations/<id>.md` if present, otherwise at
`.factory/observations/resolved/<id>.md`, on stdout.
Test: tests/binary.rs (observations_show_open_and_resolved)

WHEN `factory observations migrate` is invoked with monolithic
observation files present,
THE SYSTEM SHALL split `.factory/observations.md` and
`.factory/observations-resolved.md` into one file per observation
under `.factory/observations/<id>.md` and
`.factory/observations/resolved/<id>.md` respectively, preserving
each observation's content verbatim, and remove the monolithic files.
Test: tests/binary.rs (observations_migrate_splits_monolithic_files)
Test: src/observations.rs (migrate_splits_and_removes_monolithic)
Test: src/observations.rs (migrate_idempotent)

WHEN `factory observations migrate` is invoked with no monolithic
observation files present,
THE SYSTEM SHALL exit successfully without creating or modifying files.
Test: tests/binary.rs (observations_migrate_splits_monolithic_files)
Test: src/observations.rs (migrate_idempotent)

---

## Work Item intake and inspection

WHEN two threads create, read, and write distinct Work Items
through the same `WorkModelStore` instance,
THE SYSTEM SHALL keep each Work Item's split-storage records
consistent — no thread observes another's state, and each Work
Item's items/, attempts/, tasks/, merge-candidates/, and artifacts/
paths are written without race or partial state.
Test: src/work_model.rs (concurrent_writes_to_distinct_work_items_do_not_race)

WHEN `factory work create <id> --title <title>` is invoked from a
directory,
THE SYSTEM SHALL create `.factory/work/items/<id>.json` containing Work
Item metadata with that id and title, while Attempts, Tasks, and Merge
Candidates remain in their split collections.
Test: tests/binary.rs (work_create_writes_minimal_work_item)
Test: tests/behaviors/operations/test-work-inspection.sh (work create writes minimal Work Item)

IF `factory work create <id> --title <title>` is invoked for an
existing Work Item id,
THEN THE SYSTEM SHALL exit non-zero and leave the existing Work Item
unchanged.
Test: tests/binary.rs (work_create_refuses_existing_work_item)
Test: tests/behaviors/operations/test-work-inspection.sh (work create existing item fails)

IF `factory work create <id> --title <title>` is invoked with an invalid
Work Item id,
THEN THE SYSTEM SHALL exit non-zero and not write a Work Item file.
Test: tests/binary.rs (work_create_rejects_invalid_work_item_id)
Test: tests/behaviors/operations/test-work-inspection.sh (work create invalid id fails)

WHEN a Work Item is created through intake,
THE SYSTEM SHALL make it visible through `factory work list` and
`factory work show <id>`.
Test: tests/binary.rs (work_create_item_is_visible_through_list_and_show)
Test: tests/behaviors/operations/test-work-inspection.sh (work create item is visible)

WHEN `factory work create` is invoked with rich instructions,
THE SYSTEM SHALL persist those instructions in stored Work Item state and
make them visible through `factory work show <id>`.
Test: tests/binary.rs (work_create_persists_instructions_and_attempt_copies_them_to_write_task)
Test: tests/behaviors/operations/test-work-inspection.sh (work create persists instructions)

WHEN `factory work create` is invoked with approved planning context,
THE SYSTEM SHALL persist the brief, behaviors, approach, and plan context
in stored Work Item state and make that context visible through
`factory work show <id>` without requiring a legacy run execution
instructions file.
Test: tests/binary.rs (work_create_persists_planning_context_and_attempt_copies_it_to_write_task)

WHEN planning skills describe how to pass approved planning context to
delegated Work execution,
THE SYSTEM SHALL describe Work Item planning context through
`factory work create --brief-file --behaviors-file --approach-file
--plan-file` as the default path and SHALL confine
`.factory/runs/<run-id>/` planning files to legacy fallback or recovery
language.
Test: tests/behaviors/operations/test-planning-skills-work-context.sh

WHEN `factory work list` is invoked,
THE SYSTEM SHALL read stored Work Items from `.factory/work/items/` and
assemble Attempts, Tasks, and Merge Candidates only from their split
collections before printing each Work Item with its id and title.
Test: tests/binary.rs (work_list_outputs_stored_work_items)
Test: tests/behaviors/operations/test-work-inspection.sh (work list prints stored Work Items)

IF a `.factory/work/items/<id>.json` file contains nested Attempts,
Tasks, or Merge Candidates,
THEN THE SYSTEM SHALL ignore those nested operational collections and
expose only split-collection Attempt, Task, and Merge Candidate records.
Test: tests/work_model_external.rs (work_model_store_ignores_nested_operational_collections)

WHEN `factory work list` is invoked and no Work Items exist,
THE SYSTEM SHALL print an empty-state message and exit successfully.
Test: tests/binary.rs (work_list_empty_state_succeeds_without_work_items)
Test: tests/behaviors/operations/test-work-inspection.sh (work list prints empty state)

WHEN `factory work show <id>` is invoked for a stored Work Item,
THE SYSTEM SHALL print the Work Item as deterministic pretty JSON.
Test: tests/binary.rs (work_show_outputs_pretty_json_for_one_work_item)
Test: tests/behaviors/operations/test-work-inspection.sh (work show prints pretty JSON)

IF `factory work show <id>` is invoked for a missing Work Item,
THEN THE SYSTEM SHALL exit non-zero and report that the Work Item was
not found.
Test: tests/binary.rs (work_show_missing_item_reports_not_found)
Test: tests/behaviors/operations/test-work-inspection.sh (work show missing item fails)

WHEN `factory work abandon <id>` is invoked for a stored Work Item
without executing or reviewing Attempts, executing Tasks, reviewing
Merge Candidates, or executing Merge Candidate merges,
THE SYSTEM SHALL record durable Work Item abandonment state and persist
the supplied reason when one is provided.
Test: src/work_model.rs (abandon_records_reason_on_inactive_work_item)
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon persists reason)

IF `factory work abandon <id>` is invoked for a missing Work Item,
THEN THE SYSTEM SHALL exit non-zero, report that the Work Item was not
found, and leave Work state unchanged.
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon missing item fails)

IF `factory work abandon <id>` is invoked for a Work Item with an
executing or reviewing Attempt, executing Task, reviewing Merge
Candidate, or executing Merge Candidate merge,
THEN THE SYSTEM SHALL exit non-zero and leave Work Item state unchanged.
Test: src/work_model.rs (abandon_rejects_executing_attempt_without_changing_marker)
Test: src/work_model.rs (abandon_rejects_reviewing_attempt_without_changing_marker)
Test: src/work_model.rs (abandon_rejects_executing_task_without_changing_marker)
Test: src/work_model.rs (abandon_rejects_active_merge_candidate_without_changing_marker)
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon active item fails without state change)
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon reviewing attempt fails without state change)
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon executing task fails without state change)
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon active merge candidate fails without state change)

IF Work lifecycle commands try to plan, execute, review, or merge an
abandoned Work Item,
THEN THE SYSTEM SHALL exit non-zero and leave abandoned Work state
terminal.
Test: src/work_model.rs (abandoned_work_item_rejects_initial_attempt_planning)
Test: src/work_model.rs (abandoned_work_item_rejects_review_only_attempt_planning)
Test: src/work_model.rs (abandoned_work_item_rejects_review_task_planning)
Test: src/work_model.rs (abandoned_work_item_rejects_followup_write_planning)
Test: src/work_model.rs (abandoned_work_item_rejects_merge_candidate_planning)
Test: src/work_attempt_loop.rs (run_attempt_rejects_abandoned_work_item_without_mutating_state)
Test: src/work_task_executor.rs (run_task_rejects_abandoned_work_item_without_mutating_state)
Test: src/work_merge_executor.rs (merge_candidate_rejects_abandoned_work_item_without_mutating_state)

IF stored Work Item state contains invalid JSON, an invalid id, or a
model validation error,
THEN THE SYSTEM SHALL exit non-zero and report the invalid file or
object.
Test: tests/binary.rs (work_list_reports_invalid_stored_json_path)
Test: tests/binary.rs (work_list_reports_stored_work_item_id_mismatch)
Test: tests/binary.rs (work_list_reports_invalid_stored_work_item_id)
Test: tests/binary.rs (work_list_reports_invalid_stored_model)
Test: tests/behaviors/operations/test-work-inspection.sh (work list reports invalid stored state)

WHEN existing `.factory/runs` state exists,
THE SYSTEM SHALL keep legacy run commands working and keep Work Item
intake and inspection independent from `.factory/runs`.
Test: tests/binary.rs (work_list_empty_state_succeeds_without_work_items)
Test: tests/binary.rs (work_create_is_independent_from_legacy_runs)
Test: tests/behaviors/operations/test-work-inspection.sh (legacy runs and work inspection are independent)

WHEN `factory work attempt <work-item-id>` is invoked without an
attempt-id positional argument,
THE SYSTEM SHALL create an Attempt with id `attempt-N` where N is the
smallest positive integer such that no Attempt with that id exists on
the Work Item.
Test: src/work_model.rs (next_attempt_id_empty_returns_attempt_1)
Test: src/work_model.rs (next_attempt_id_sequential_returns_next)
Test: src/work_model.rs (next_attempt_id_with_gap_returns_smallest_unused)
Test: src/work_model.rs (next_attempt_id_ignores_non_numeric_ids)
Test: tests/binary.rs (work_attempt_auto_id_creates_attempt_1)
Test: tests/binary.rs (work_attempt_auto_id_sequential_creates_attempt_2)
Test: tests/binary.rs (work_attempt_auto_id_fills_gap)

WHEN `factory work attempt <work-item-id> <attempt-id>` is invoked with
an explicit attempt-id,
THE SYSTEM SHALL create the Attempt with that exact id.
Test: tests/binary.rs (work_attempt_adds_planned_attempt_with_initial_write_task)
Test: tests/binary.rs (work_attempt_explicit_id_still_works)

WHEN `factory work attempt run <work-item-id>` is invoked without an
attempt-id,
THE SYSTEM SHALL operate on the most recently created Attempt of the
Work Item.
Test: src/work_model.rs (latest_attempt_id_returns_last)

WHEN `factory work attempt run <work-item-id> <attempt-id>` is invoked
with an explicit attempt-id,
THE SYSTEM SHALL operate on that exact Attempt.
Test: tests/binary.rs (work_attempt_explicit_id_still_works)

WHEN `factory work merge <work-item-id>` is invoked without a
merge-candidate-id,
THE SYSTEM SHALL operate on the most recently created Merge Candidate
of the Work Item.
Test: src/work_model.rs (latest_merge_candidate_id_returns_last)

WHEN `factory work merge <work-item-id> <merge-candidate-id>` is
invoked with an explicit merge-candidate-id,
THE SYSTEM SHALL use that exact Merge Candidate.
Untestable: No binary test exercises explicit merge-candidate-id; implicit latest-candidate path is tested

IF `factory work attempt run <work-item-id>` is invoked without an
attempt-id and no Attempts exist on the Work Item,
THEN THE SYSTEM SHALL exit non-zero with an error message explaining
no Attempt exists to run.
Test: tests/binary.rs (work_attempt_run_no_attempts_reports_error)

IF `factory work merge <work-item-id>` is invoked without a
merge-candidate-id and no Merge Candidates exist on the Work Item,
THEN THE SYSTEM SHALL exit non-zero with an error message explaining
no Merge Candidate exists to merge.
Test: tests/binary.rs (work_merge_no_candidates_reports_error)

WHEN `factory work attempt <work-item-id> <attempt-id>` is invoked for a
stored Work Item,
THE SYSTEM SHALL append a planned Attempt under that Work Item and create
one initial scheduler-facing `write` Task for the Attempt.
Test: tests/binary.rs (work_attempt_adds_planned_attempt_with_initial_write_task)
Test: tests/behaviors/operations/test-work-attempt-intake-review.sh (attempt adds planned Attempt)
Test: tests/behaviors/operations/test-work-attempt-intake-review.sh (attempt adds one initial write Task)

WHEN `factory work attempt <work-item-id> <attempt-id>` creates the
initial `write` Task,
THE SYSTEM SHALL give the Task role `author`, id `<attempt-id>-write`,
the matching Work Item and Attempt ids, and exactly one writable
workspace reference at
`../work-<work-item-id-byte-len>-<work-item-id>-<attempt-id>`, without
creating or executing that workspace during Attempt creation.
Test: tests/work_model_external.rs (work_item_add_initial_attempt_creates_scheduler_facing_write_task)
Test: tests/binary.rs (work_attempt_adds_planned_attempt_with_initial_write_task)
Test: tests/behaviors/operations/test-work-attempt-intake-review.sh (initial write Task has ids and one writable workspace)
Test: tests/behaviors/operations/test-work-attempt-intake-review.sh (work show prints Attempt and Task as pretty JSON)

WHEN `factory work attempt <work-item-id> <attempt-id>` creates the
initial `write` Task for a Work Item with instructions,
THE SYSTEM SHALL copy those instructions to the Task so Task execution
can build the coder prompt from durable Work model state.
Test: tests/binary.rs (work_create_persists_instructions_and_attempt_copies_them_to_write_task)
Test: tests/behaviors/operations/test-work-task-instructions.sh (attempt copies instructions to initial write Task)

WHEN `factory work attempt <work-item-id> <attempt-id>` creates the
initial `write` Task for a Work Item with planning context and no
explicit instructions,
THE SYSTEM SHALL derive Task instructions from the Work Item planning
context so Task execution can build the coder prompt from durable Work
model state.
Test: tests/binary.rs (work_create_persists_planning_context_and_attempt_copies_it_to_write_task)

WHEN `factory work task run <work-item-id> <attempt-id> <task-id>` is
invoked for an existing planned `write` Task with exactly one writable
workspace,
THE SYSTEM SHALL create or reuse a registered git worktree at that
workspace path and launch the selected coder in that workspace.
Test: tests/binary.rs (work_task_run_completes_write_task_with_committed_output)
Test: tests/behaviors/operations/test-work-task-run.sh (run reuses worktree and launches coder there)

WHEN Factory runs a Work-model behavior review Task for an Attempt whose
Work Item includes a behavior increment,
THE SYSTEM SHALL include the behavior increment explicitly in the review
Task prompt.
Test: tests/binary.rs (work_behavior_review_task_prompt_includes_behavior_increment)

WHEN Factory runs a Work-model behavior review Task for an Attempt whose
Work Item does not include a behavior increment,
THE SYSTEM SHALL state in the review Task prompt that no Work behavior
increment was provided.
Test: tests/binary.rs (work_behavior_review_task_prompt_states_missing_behavior_increment)

WHEN `factory work task run <work-item-id> <attempt-id> <task-id>` or
`factory work attempt run <work-item-id> <attempt-id>` launches a
`write` Task with stored Task instructions,
THE SYSTEM SHALL include those instructions in the coder prompt.
Test: tests/binary.rs (work_task_run_includes_task_instructions_in_coder_prompt)
Test: tests/binary.rs (work_task_run_includes_planning_context_in_coder_prompt)
Test: tests/behaviors/operations/test-work-task-run.sh (run passes Task instructions to coder prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (task run uses durable instructions and keeps extra args out of prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (attempt run uses durable instructions and keeps extra args out of prompt)

WHEN `factory work task run <work-item-id> <attempt-id> <task-id>` or
`factory work attempt run <work-item-id> <attempt-id>` launches a
`write` Task,
THE SYSTEM SHALL tell the coder that the Task completes only after all
Task output is committed and the writable workspace is clean.
Test: tests/binary.rs (work_task_run_passes_task_context_to_coder_prompt)
Test: tests/behaviors/operations/test-work-task-run.sh (run passes Task context to coder prompt)

WHEN `factory work task run <work-item-id> <attempt-id> <task-id>` or
`factory work attempt run <work-item-id> <attempt-id>` launches a
`write` Task,
THE SYSTEM SHALL tell the author to perform an upfront scope preflight
that identifies likely touched behavior statements, user-facing docs,
tests, skills/expertise, and verification commands before editing; to
update applicable related artifacts when changing a user-facing command,
behavior, skill, or documentation surface; and to record why related
artifacts do not apply when the Task is intentionally code-only or
docs-only.
Test: tests/binary.rs (work_task_run_passes_task_context_to_coder_prompt)

WHEN Factory launches a Work-model follow-up `write` Task that includes
input artifacts,
THE SYSTEM SHALL tell the author to read the review input artifacts
first, address the concrete findings, and check whether each finding
reveals a missing first-pass preflight item.
Test: tests/binary.rs (work_attempt_run_exposes_followup_input_artifacts)

WHEN Factory launches a Work-model `review` Task that includes input
artifacts,
THE SYSTEM SHALL name the input artifact paths in the review prompt and
tell the reviewer to read them first before evaluating the candidate.
Test: tests/binary.rs (work_task_run_completes_review_task_with_fail_verdict_artifact)

WHEN Factory launches a Work-model `review` Task,
THE SYSTEM SHALL name the Work review artifact path, the exact
filesystem `review.md` path the reviewer must write, and the reviewer
artifact directory; SHALL tell the reviewer that the candidate's
existing build outputs are readable; SHALL tell the reviewer that the
reviewer artifact directory has been pre-populated with the writer's
build outputs for warm-start incremental builds; SHALL include Cargo
guidance to set `CARGO_TARGET_DIR` under the reviewer artifact
directory; and SHALL NOT instruct the reviewer to write legacy
`.factory/runs/<run-id>/reviews/...` artifacts.
Test: src/work_task_executor.rs (work_review_prompt_names_work_artifacts_and_writable_outputs)

WHEN Factory plans to launch an Attempt-time review Task and the
candidate workspace contains a recognized toolchain marker file
(`Cargo.toml`, `package.json`, `pom.xml`, or `build.gradle`),
THE SYSTEM SHALL copy that toolchain's canonical build directories from
the candidate workspace into the reviewer's artifact directory before
launching the reviewer.
Test: src/prep.rs (copies_existing_dirs_and_skips_missing)
Test: src/prep.rs (copies_multiple_node_dirs)

WHEN Factory performs the warm-cache copy,
THE SYSTEM SHALL try a reflink copy first (`cp -c` on macOS,
`cp --reflink=auto` on Linux), fall back to a hardlink copy (`cp -l`)
if reflinks are unsupported, and fall back to a deep copy as a last
resort.
Test: src/prep.rs (copies_existing_dirs_and_skips_missing)

WHEN Factory copies a build directory that does not exist in the
candidate workspace,
THE SYSTEM SHALL skip that directory without error.
Test: src/prep.rs (no_error_when_all_dirs_missing)
Test: src/prep.rs (copies_existing_dirs_and_skips_missing)

WHEN `.factory/hooks/prepare-pre-review` exists and is executable in
the candidate workspace,
THE SYSTEM SHALL run that hook instead of the built-in auto-prep, with
`FACTORY_REVIEWER_ARTIFACT_DIR` set in the env and CWD = candidate
workspace.
Test: src/hooks.rs (passes_reviewer_artifact_dir_via_env)

WHEN the candidate workspace contains neither a recognized toolchain
marker nor a `prepare-pre-review` hook,
THE SYSTEM SHALL launch the reviewer without any pre-population.
Test: src/prep.rs (returns_none_when_no_marker)

WHEN Factory launches a Work-model `review` Task for a candidate
workspace,
THE SYSTEM SHALL include a `git -C <candidate-workspace> diff <range>`
review diff command that shell-quotes the resolved candidate workspace
path and exact revision range so the command can execute through
`/bin/sh`.
Test: src/work_task_executor.rs (work_review_prompt_includes_shell_safe_executable_diff_command)
Test: src/review_diff_command.rs (review_diff_command_survives_apostrophes_through_sh)

WHEN a behavior operation script invokes the Factory binary,
THE SYSTEM SHALL allow callers to set `FACTORY_BIN_OVERRIDE` to an
explicit binary path; when no override is set, the script SHALL keep the
repository-local `target/debug/factory` default.
Test: tests/behaviors/operations/test-behavior-bin-override.sh (operation scripts use FACTORY_BIN_OVERRIDE for debug binary bindings)

IF a caller passes extra args to `factory work task run` or
`factory work attempt run`,
THE SYSTEM SHALL pass those args only as coder options and SHALL NOT
treat them as additional task prompt content.
Test: tests/binary.rs (work_task_run_keeps_extra_args_out_of_task_prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (task run uses durable instructions and keeps extra args out of prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (attempt run uses durable instructions and keeps extra args out of prompt)

WHEN a Work Item has no explicit instructions,
THE SYSTEM SHALL preserve the minimal write Task prompt and SHALL NOT
include a `Task instructions:` section.
Test: tests/binary.rs (work_task_run_passes_task_context_to_coder_prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (minimal Work Item keeps minimal prompt)

WHEN a write Task coder exits successfully,
THE SYSTEM SHALL complete the Task only if the writable workspace is
clean and contains at least one commit produced after Factory created or
bound the workspace for that Task run.
Test: tests/binary.rs (work_task_run_completes_write_task_with_committed_output)
Test: tests/binary.rs (work_task_run_rejects_reused_workspace_without_new_commit)
Test: tests/behaviors/operations/test-work-task-run.sh (clean committed Task completes)
Test: tests/behaviors/operations/test-work-task-run.sh (success without new commit fails with guidance)

IF the write Task coder exits successfully but the writable workspace has
uncommitted changes,
THEN THE SYSTEM SHALL leave the Task incomplete and report that the Task
must commit or remove the dirty changes.
Test: tests/binary.rs (work_task_run_rejects_dirty_successful_workspace)
Test: tests/behaviors/operations/test-work-task-run.sh (dirty successful Task fails with guidance)

IF the write Task coder exits successfully but the writable workspace has
no committed Task output produced by that run,
THEN THE SYSTEM SHALL leave the Task incomplete and report that there is
no committed Task output.
Test: tests/binary.rs (work_task_run_rejects_success_without_commits)
Test: tests/binary.rs (work_task_run_rejects_reused_workspace_without_new_commit)
Test: tests/behaviors/operations/test-work-task-run.sh (success without new commit fails with guidance)

WHEN `factory work review <work-item-id> <attempt-id>` is invoked for an
Attempt with completed write output,
THE SYSTEM SHALL append planned `review` Tasks that read the candidate
workspace, declare no writable workspace, and declare artifact areas
under `.factory/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`.
Test: tests/binary.rs (work_review_plans_review_tasks_for_completed_attempt)
Test: tests/behaviors/operations/test-work-task-run.sh (review planning adds read-only Task without changing candidate)

IF `factory work review <work-item-id> <attempt-id>` is invoked for an
Attempt without completed write output,
THEN THE SYSTEM SHALL exit non-zero and leave stored Work Item state
unchanged.
Test: tests/binary.rs (work_review_requires_completed_write_output)
Test: tests/behaviors/operations/test-work-task-run.sh (review planning requires completed write output)

WHEN `factory work review-codebase <work-item-id> <attempt-id>` is
invoked for a stored Work Item with no existing Attempt of that id,
THE SYSTEM SHALL append a review-only Attempt with planned review Tasks
for the default reviewer set.
Test: tests/binary.rs (work_review_codebase_creates_review_only_attempt)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-codebase creates review-only Attempt)

WHEN a review-only Attempt is created,
THE SYSTEM SHALL give each review Task read-only access to the current
source checkout and a managed artifact area under
`.factory/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`.
Test: tests/binary.rs (work_review_codebase_creates_review_only_attempt)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-codebase creates review-only Attempt)

IF `factory work review-codebase <work-item-id> <attempt-id>` is invoked
for a missing Work Item or duplicate Attempt id,
THEN THE SYSTEM SHALL exit non-zero without changing Work state.
Test: tests/binary.rs (work_review_codebase_missing_or_duplicate_leaves_state_unchanged)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-codebase rejects missing and duplicate)

WHEN `factory work task run <work-item-id> <attempt-id> <task-id>` is
invoked for a planned `review` Task,
THE SYSTEM SHALL complete the Task after the reviewer writes `review.md`
under the Task artifact area, even when that artifact contains
`Verdict: fail` or `Verdict: uncertain`.
Test: tests/binary.rs (work_task_run_completes_review_task_with_fail_verdict_artifact)
Test: tests/behaviors/operations/test-work-task-run.sh (review Task with fail verdict completes)
Test: tests/behaviors/operations/test-work-task-run.sh (review Task with uncertain verdict completes)

WHEN `factory work attempt run <work-item-id> <attempt-id>` is invoked
for an Attempt with a planned write Task,
THE SYSTEM SHALL run the write Task through the existing Task executor,
then reload stored Work Item state before planning later transitions.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

WHEN `factory work attempt run <work-item-id> <attempt-id>` advances a
normal Work Attempt whose initial write Task has completed and no review
round is planned for that write output,
THE SYSTEM SHALL plan initial review Tasks using the full Work reviewer
set and run planned review Tasks through the existing Task executor.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

WHEN a normal Work Attempt completes a write round created from
failed review artifacts,
THE SYSTEM SHALL plan the next review round only for the failed reviewer
roles that fed that write round.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_mixed_failed_and_uncertain_reviews)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop plans follow-up with mixed missing review)

WHEN a normal Work Attempt completes a write round created from
failed review artifacts and Factory plans a targeted follow-up review
round,
THE SYSTEM SHALL attach the relevant prior failed review artifact for
each planned review Task role as that review Task's input artifact.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_mixed_failed_and_uncertain_reviews)

IF a normal Work Attempt completes a write round and the failed
reviewer roles cannot be derived from the follow-up Task input artifacts,
THEN THE SYSTEM SHALL plan the next review round using the full Work
reviewer set.
Test: src/work_attempt_loop.rs (unmappable_followup_inputs_fall_back_to_full_review_roles)

WHEN `factory work attempt run <work-item-id> <attempt-id>` is invoked
for a normal Work Attempt with planned review Tasks,
THE SYSTEM SHALL run the planned review Tasks in parallel with
concurrency limited to `FACTORY_MAX_PARALLEL_REVIEWERS` (default 5,
minimum 1) before planning later transitions.
Test: src/work_attempt_loop.rs (cap_enforcement_limits_in_flight_reviewers)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

WHEN `factory work attempt run <work-item-id> <attempt-id>` is invoked
for a review-only Attempt with planned review Tasks,
THE SYSTEM SHALL run the planned review Tasks serially because
review-only reviewers share a source checkout.
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop runs planned review Tasks)

WHEN the planned review-task count for a round exceeds the configured
`FACTORY_MAX_PARALLEL_REVIEWERS` cap,
THE SYSTEM SHALL queue excess tasks and launch each as in-flight slots
free up, keeping at most `cap` reviewer threads in flight at any time.
Test: src/work_attempt_loop.rs (cap_enforcement_limits_in_flight_reviewers)

WHEN `factory work merge <work-item-id> <merge-candidate-id>` finishes
the fast-forward,
THE SYSTEM SHALL append an entry to the post-merge review queue at
`.factory/work/post-merge-review-queue.json` recording the target
branch, merged commit, timestamp, source Work Item, and source Merge
Candidate, then spawn a detached `factory work post-merge-review run`
child that sleeps the debounce window before reviewing. The merge
command SHALL return immediately after spawning the child; no LLM
reviewers run inside `factory work merge`.
Untestable: Requires detached process spawn with debounce timing and queue file coordination

WHEN `factory work post-merge-review run` runs,
THE SYSTEM SHALL sleep the debounce window, then for each target
branch with a queued entry at least `debounce_seconds` old, run a
review-only Attempt against the target branch's current HEAD using
the full reviewer set, and clear processed queue entries.
Untestable: Requires end-to-end post-merge runner with debounce sleep and LLM reviewer set

WHEN multiple merges arrive for the same target branch within the
debounce window,
THE SYSTEM SHALL coalesce them — only the latest entry triggers a
review; earlier detached children wake up, see a newer entry, and
exit. The single review covers the cumulative range.
Untestable: Requires concurrent detached processes racing on debounce window timing

WHEN the post-merge review finds any reviewer artifact with a failing
or uncertain verdict,
THE SYSTEM SHALL create a post-merge-review-fix Work Item with the
failed review artifacts as planning context, run its first Attempt,
and on a successful Merge Candidate auto-invoke `factory work merge`.
The auto-merge spawns its own detached post-merge review with
`FACTORY_POST_MERGE_REVIEW_FIX_DEPTH` incremented; recursion stops at
`FACTORY_MAX_POST_MERGE_REVIEW_FIX_DEPTH` (default 5).
Untestable: Requires end-to-end post-merge review with LLM reviewers, auto-fix, and recursive depth tracking

WHEN the post-merge review runner spawns a review-only Attempt
against the source checkout for a synthetic `post-merge-<branch>-<short>`
Work Item,
THE SYSTEM SHALL apply a non-restoring guard that checks
source-HEAD-still-matches-the-merged-commit on completion but does
NOT snapshot or restore non-Factory worktree changes or protected
`.factory/` file contents.
Test: tests/binary.rs (post_merge_review_guard_allows_source_changes)
Test: tests/binary.rs (post_merge_review_guard_allows_factory_state_changes)
Test: tests/binary.rs (post_merge_review_preflight_allows_non_factory_worktree_changes)
Test: src/work_task_executor.rs (post_merge_source_guard_finish_succeeds_with_worktree_edits)
Test: src/work_task_executor.rs (post_merge_source_guard_finish_succeeds_with_factory_mutations)

WHEN the source HEAD moves during a post-merge review (e.g., the
user lands another merge concurrently),
THE SYSTEM SHALL mark the review tasks failed with a clear error
explaining the source HEAD changed, leave the merged-commit's queue
entry in place so the next post-merge runner can re-attempt, and
SHALL NOT attempt to restore the head.
Test: tests/binary.rs (post_merge_review_guard_fails_when_head_moves)
Test: src/work_task_executor.rs (post_merge_source_guard_finish_fails_when_head_moves)

WHEN `factory work review-codebase` is invoked interactively by the
user against the current source checkout,
THE SYSTEM SHALL apply the existing restorative guard semantics:
snapshot non-Factory worktree state and protected `.factory/`
contents at begin, restore both on finish if reviewers modified
them, and surface clear errors when restoration was needed.
Test: tests/binary.rs (work_attempt_run_review_only_rejects_source_changes)
Test: tests/binary.rs (work_attempt_run_review_only_rejects_factory_state_changes)
Test: tests/binary.rs (work_attempt_run_review_only_restores_mixed_source_and_factory_changes)

WHEN a reviewer task running under either guard variant modifies a
file inside its allowed reviewer artifact directory,
THE SYSTEM SHALL leave that change in place (artifact directories
are the reviewer's writable surface, unchanged from today).
Test: tests/binary.rs (post_merge_review_guard_passes_clean_review)
Test: tests/binary.rs (work_attempt_run_review_only_passes_without_merge_candidate)

IF the post-merge review's review-only Attempt completes with at
least one reviewer task whose review.md has `Verdict: fail` or
`Verdict: uncertain`,
THEN THE SYSTEM SHALL collect those review artifacts and proceed to
auto-create a `post-merge-review-fix-<branch>-<timestamp>` Work
Item, regardless of whether peer reviewer tasks are in
`failed`/`needs-user`/`complete` status.
Untestable: Requires end-to-end post-merge review Attempt with mixed reviewer task states

WHEN the post-merge review reads completed review artifacts to
decide whether to create a forward-fix Work Item,
THE SYSTEM SHALL include reviewers whose Task status is `failed`
in addition to `complete`, treating any reviewer that wrote a
review.md with a non-pass verdict as a finding source.
Untestable: Requires end-to-end post-merge review with failed-status reviewer artifacts

IF the post-merge review cannot create a forward-fix Work Item
(e.g., because storage write fails),
THEN THE SYSTEM SHALL log the failure to the post-merge review log
and leave the synthetic Work Item state intact so an operator can
inspect the findings manually.
Untestable: Requires storage write failure injection during post-merge forward-fix creation

WHEN `factory cleanup` runs and finds a sibling directory matching
`../review-<bytelen>-<work-item-id>-<attempt-id>-<reviewer>` whose
Work Item has no merge candidate currently executing,
THE SYSTEM SHALL list the directory in the dry-run report; with
`--apply`, THE SYSTEM SHALL remove the directory and any registered
git worktree pointing at it.
Test: src/cleanup.rs (parse_reviewer_worktree_name_extracts_components, parse_reviewer_worktree_name_handles_long_work_item_id, parse_reviewer_worktree_name_rejects_non_reviewer_suffix, parse_reviewer_worktree_name_rejects_non_matching_names, stranded_reviewer_worktree_detected_for_non_executing_work_item, stranded_reviewer_worktree_preserved_for_executing_merge_candidate, stranded_reviewer_worktree_removed_on_apply)

WHEN all review Tasks for an Attempt review round complete and all
review artifacts have passing verdicts,
THE SYSTEM SHALL mark the Attempt review state as `passed`, leave the
Attempt `complete`, create one durable Merge Candidate, and report the
Merge Candidate id.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

WHEN `factory work attempt run <work-item-id> <attempt-id>` is invoked
for a review-only Attempt with planned review Tasks,
THE SYSTEM SHALL run those reviewer Tasks, require each reviewer to write
its Work review artifact, and leave non-Factory source files unchanged.
Test: tests/binary.rs (work_attempt_run_review_only_passes_without_merge_candidate)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only pass completes without Merge Candidate)

WHEN a review-only review Task starts,
THE SYSTEM SHALL require the source checkout HEAD to match the source
commit recorded in the Task review context.
Test: tests/binary.rs (work_attempt_run_review_only_requires_recorded_source_commit)

WHEN a review-only review Task changes source files or Factory-owned
state outside its managed artifact area,
THE SYSTEM SHALL mark the Task failed, restore changed non-Factory
source files, and report that the source checkout changed outside the
allowed boundary.
Test: tests/binary.rs (work_attempt_run_review_only_rejects_source_changes)
Test: tests/binary.rs (work_attempt_run_review_only_rejects_factory_state_changes)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only rejects source changes)

WHEN all review-only reviewer artifacts have verdict `pass`,
THE SYSTEM SHALL mark the Attempt complete with review state `passed`
and SHALL NOT create a Merge Candidate.
Test: tests/binary.rs (work_attempt_run_review_only_passes_without_merge_candidate)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only pass completes without Merge Candidate)

WHEN any review-only reviewer artifact has verdict `fail`,
THE SYSTEM SHALL mark the Attempt failed with review state `failed` and
SHALL NOT create a write round or Merge Candidate.
Test: tests/binary.rs (work_attempt_run_review_only_fails_without_followup)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only fail stops without follow-up)

WHEN any review-only reviewer artifact has verdict `uncertain` and none
has verdict `fail`,
THE SYSTEM SHALL mark the Attempt `needs-user`, write a Work handoff
artifact, and SHALL NOT create a write round or Merge
Candidate.
Test: tests/binary.rs (work_attempt_run_review_only_uncertain_needs_user)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only uncertain needs user)

WHEN a Merge Candidate is created from a passed Attempt,
THE SYSTEM SHALL record the source candidate workspace, target workspace,
source branch, and candidate commit from the latest completed write Task,
initialize the target branch from that write Task's source branch, and set
the Merge Candidate review state to pending.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

IF `factory work attempt run <work-item-id> <attempt-id>` is invoked for
an Attempt whose reviews already passed and whose Merge Candidate already
exists,
THEN THE SYSTEM SHALL leave Work Item state unchanged and report the
existing Merge Candidate.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop invalid or terminal request leaves state unchanged)

WHEN `factory work merge-candidate <work-item-id> <merge-candidate-id>`
is invoked,
THE SYSTEM SHALL print the stored Merge Candidate as pretty JSON.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

IF `factory work merge-candidate <work-item-id> <merge-candidate-id>` is
invoked for a missing Work Item or missing Merge Candidate,
THEN THE SYSTEM SHALL exit non-zero and leave Work Item state unchanged.
Test: tests/binary.rs (work_merge_candidate_missing_item_or_candidate_reports_error)

WHEN `factory work merge <work-item-id> <merge-candidate-id>` is invoked
for a stored Merge Candidate that still needs to land,
THE SYSTEM SHALL update the candidate workspace against the target
branch, run configured pre-merge checks, run the full merge-time reviewer
set, and fast-forward the target branch only after those steps pass.
Test: tests/binary.rs (work_merge_candidate_lands_after_merge_time_reviews)

WHEN `factory work merge <work-item-id> <merge-candidate-id>` launches a
merge-time reviewer for a Work Merge Candidate,
THE SYSTEM SHALL name the exact
`.factory/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/reviews/<role>/review.md`
artifact as the review output, provide the absolute filesystem path the
reviewer must write, and SHALL NOT instruct the reviewer to write legacy
`.factory/runs/<run-id>/reviews/...` artifacts.
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge lands after update, checks, and reviewers)

WHEN Factory runs a merge-time behavior reviewer for a Merge Candidate
whose Work Item includes a behavior increment,
THE SYSTEM SHALL include the behavior increment explicitly in the merge
review prompt.
Test: tests/binary.rs (work_merge_candidate_lands_after_merge_time_reviews)

WHEN Factory runs a merge-time behavior reviewer for a Merge Candidate
whose Work Item does not include a behavior increment,
THE SYSTEM SHALL state in the merge review prompt that no Work behavior
increment was provided.
Test: tests/binary.rs (work_merge_behavior_review_prompt_states_missing_behavior_increment)

WHEN `factory work merge <work-item-id> <merge-candidate-id>` builds the
system prompt for a Work merge-time reviewer,
THE SYSTEM SHALL use the reviewer prompt's `[work-system]` section when
one exists, fall back to the raw `[system]` section otherwise, then tell
the reviewer to follow the candidate workspace's absolute
`skills/review-<role>/SKILL.md` path when that skill exists, or to apply
the reviewer role directly when it does not. If the candidate workspace
contains `.factory/expertise/decisions.md`, the system shall name that
absolute path as the recorded-decisions file.
Test: src/work_merge_executor.rs (merge_reviewer_system_prompt_uses_work_section_without_legacy_filtering)
Test: tests/binary.rs (work_merge_candidate_lands_after_merge_time_reviews)

WHEN a merge-time reviewer receives a candidate workspace,
THE SYSTEM SHALL tell the reviewer that the candidate workspace is
read-only for review purposes and that scratch tests, suggested patches,
or proposed documentation edits belong in merge review artifacts rather
than in the candidate workspace.
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge lands after update, checks, and reviewers)

WHEN Factory launches a merge-time Work reviewer,
THE SYSTEM SHALL include a `git -C <candidate-workspace> diff <range>`
review diff command that shell-quotes the candidate workspace path and
the exact target-to-candidate commit range.
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge lands after update, checks, and reviewers)

WHEN merge checks have already run before merge-time reviewers,
THE SYSTEM SHALL tell reviewers that checks ran before reviewers without
presenting merge-check artifact paths as required reviewer inputs.
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge lands after update, checks, and reviewers)

WHEN `factory work merge <work-item-id> <merge-candidate-id>` is invoked
for a Merge Candidate with merge status `merged` and a stored
`merged_commit`,
THE SYSTEM SHALL report the stored merged commit without resolving
workspaces, rebasing, running checks, running reviewers, or moving the
target branch.
Test: tests/binary.rs (work_merge_candidate_rerun_after_cleanup_preserves_landed_state)

IF `factory work merge <work-item-id> <merge-candidate-id>` is invoked
for a Merge Candidate whose stored provenance no longer matches the
passed Attempt output,
THEN THE SYSTEM SHALL leave the target branch and stored Merge Candidate
state unchanged.
Test: tests/binary.rs (work_merge_candidate_rejects_stale_stored_provenance_without_rewrite)

WHEN `factory work merge <work-item-id> <merge-candidate-id>` reaches
the rebase step,
THE SYSTEM SHALL invoke an agent to perform `git rebase <target>` inside
the candidate workspace and produce a rebased candidate-tip commit,
regardless of whether conflicts would have arisen from a non-agentic
rebase.
Test: tests/binary.rs (work_merge_candidate_rebases_when_target_advanced)

WHEN the rebase step is invoked,
THE SYSTEM SHALL record the rebase as a Task on the Attempt with its own
ID, kind `rebase`, artifact directory, prompt log, and status, visible
via `factory work show <work-item-id>`.
Test: tests/binary.rs (work_merge_candidate_rebases_when_target_advanced)

WHEN the rebase agent encounters one or more conflicts,
THE SYSTEM SHALL provide the agent with the conflicting files' content
and the conflict markers, and SHALL allow the agent to resolve those
conflicts in-place, mark them resolved with `git add`, and continue the
rebase to completion.
Test: tests/binary.rs (work_merge_rebase_resolves_trivial_conflict)

IF the rebase agent reports it cannot resolve the conflicts,
THEN THE SYSTEM SHALL transition the Merge Candidate to `needs-user`,
attach the conflict context to the rebase Task's artifact directory, and
exit without modifying the target branch.
Test: tests/binary.rs (work_merge_rebase_gives_up_transitions_to_needs_user)
Test: tests/binary.rs (work_merge_candidate_rebase_failure_leaves_target_unchanged)

IF the rebase agent exits non-zero without writing `give-up.md`,
THEN THE SYSTEM SHALL mark the rebase Task as `failed`, transition the
Merge Candidate to `failed`, and exit without modifying the target
branch.
Test: tests/binary.rs (work_merge_rebase_agent_crash_without_give_up_fails)

WHEN the rebase agent completes the rebase successfully,
THE SYSTEM SHALL set the new candidate-tip SHA on the Merge Candidate
(`candidate_commit`), on every completed Write Task's `output.commit`,
and on the Attempt's `artifacts[*].path` entries for those Tasks.
Per-task SHA fidelity is intentionally lossy; per-task contribution
remains visible through the Attempt's Task list and artifact directories.
Test: tests/binary.rs (work_merge_rebase_provenance_updated_after_rebase)
Test: src/work_merge_executor.rs (regenerate_provenance_updates_all_write_tasks_and_candidate)
Test: src/work_merge_executor.rs (regenerate_provenance_leaves_non_write_tasks_unchanged)

WHEN the rebase agent finishes resolving conflicts and before committing
each resolution,
THE SYSTEM SHALL NOT invoke project hooks (e.g., format, lint).
Post-rebase cleanup of the candidate state remains the responsibility of
`fix-pre-merge`.
Untestable: Requires LLM rebase agent resolving real conflicts to verify no hook invocation

WHEN the rebase step completes successfully,
THE SYSTEM SHALL proceed to `check-pre-merge` and `fix-pre-merge`
unchanged from current behavior, and SHALL NOT run any review Tasks
between the rebase Task and the fast-forward.
Test: tests/binary.rs (work_merge_candidate_rebases_when_target_advanced)

WHEN the rebase Task or a subsequent merge step fails and the user
resolves the underlying issue, then re-runs `factory work merge` for the
same Merge Candidate,
THE SYSTEM SHALL re-run the rebase step from the candidate workspace in
its current state and SHALL NOT reject the candidate solely because
earlier provenance pointers were updated.
Test: src/work_merge_executor.rs (next_rebase_task_id_increments)

IF the target branch moves after merge checks and reviewers run but
before the fast-forward merge,
THEN THE SYSTEM SHALL reject the merge, preserve the moved target branch,
and record merge status `failed` with a failure reason on the stored
Merge Candidate.
Test: tests/binary.rs (work_merge_candidate_rejects_target_moved_during_review)

WHEN `factory work merge <work-item-id> <merge-candidate-id>` lands a
Merge Candidate,
THE SYSTEM SHALL record merge status `merged`, the merged commit, and
merge-time review artifacts on the stored Merge Candidate, then remove the
managed candidate worktree. If worktree cleanup fails after merging, the
system shall warn without changing the merged merge state.
Test: tests/binary.rs (work_merge_candidate_lands_after_merge_time_reviews)

IF merge-time reviewers fail while `factory work merge <work-item-id>
<merge-candidate-id>` executes and the same-invocation follow-up
write budget (`MAX_MERGE_FOLLOWUP_WRITES_PER_INVOCATION = 2`) is
exhausted,
THEN THE SYSTEM SHALL leave the target branch unchanged, record merge
status `needs-user`, review state `failed`, a failure reason naming
the exhausted budget, and review artifacts on the stored Merge
Candidate, and write a `needs-user.md` handoff under the merge
artifact directory naming the failed review artifact paths.
Test: src/work_merge_executor.rs (failed_review_paths_picks_only_fail_and_uncertain_verdicts)
Test: src/work_merge_executor.rs (write_merge_needs_user_handoff_lists_failed_review_paths)

WHEN merge-time reviewers return any fail or uncertain verdict and the
same-invocation write-round budget permits another cycle,
THE SYSTEM SHALL invoke the configured Coder against the candidate
workspace with the failed merge-time review artifact paths as input
artifacts, ask the coder to address the findings and commit, verify
the workspace is clean and new commits were produced, then restart
the merge loop from rebase, checks, and merge-time review.
Untestable: Requires end-to-end merge with LLM reviewer verdicts and follow-up write cycle

IF merge-time reviewer execution panics, launch-fails, or returns a
non-verdict error,
THEN THE SYSTEM SHALL leave the target branch unchanged, record merge
status `failed`, review state `failed`, the underlying error as the
failure reason, and the partial review artifacts on the stored Merge
Candidate. The merge loop SHALL NOT retry these non-verdict failures.
Untestable: Requires reviewer panic or launch failure during live merge execution

IF a merge-time reviewer modifies, stages, unstages, or creates files in
the candidate workspace while `factory work merge <work-item-id>
<merge-candidate-id>` executes,
THEN THE SYSTEM SHALL stop before merging, leave the target branch
unchanged, record the reviewer as non-passing, and expose an error that
names the reviewer and dirty candidate workspace. This includes ignored
files such as files under the candidate workspace's `.factory` tree.
Test: tests/binary.rs (work_merge_candidate_dirty_reviewer_fails_before_merging)
Test: tests/binary.rs (work_merge_candidate_dirty_ignored_reviewer_fails_before_merging)
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge dirty reviewer leaves target unchanged)
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge dirty Factory state reviewer leaves target unchanged)

IF configured pre-merge checks fail while `factory work merge
<work-item-id> <merge-candidate-id>` executes,
THEN THE SYSTEM SHALL leave the target branch unchanged and record merge
status `failed`, a failure reason, and check artifacts on the stored
Merge Candidate.
Test: tests/binary.rs (work_merge_candidate_failed_check_leaves_target_unchanged)

WHEN any completed review artifact has a failing verdict and the
Attempt loop's no-progress detector and total-rounds ceiling both
permit another cycle,
THE SYSTEM SHALL mark the Attempt review state as `failed` and create
a planned write round with deterministic id
`<attempt-id>-write-<n>` (n = count of existing write Tasks + 1), the
candidate workspace as writable access, write Task instructions
copied from explicit Work Item instructions or derived from the Work
Item planning context, and the failed review artifacts as Task
inputs.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_failed_reviews)
Test: tests/binary.rs (work_create_planning_context_feeds_followup_for_failed_reviews)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop plans follow-up write)

WHEN every reviewer in the next planned review round receives the same
role's `review.md` from the prior completed round as an input
artifact,
THE SYSTEM SHALL render that prior review in the reviewer's prompt
framed as "a previous review of this candidate" and instruct the
reviewer to report a `Progress:` field (`yes`, `no`, `partial`, or
`first-pass`) alongside its `Verdict:`.
Test: tests/binary.rs (work_task_run_completes_review_task_with_fail_verdict_artifact)

WHEN the Attempt loop sees that every completed review in each of the
last `FACTORY_MAX_NO_PROGRESS_ROUNDS` consecutive review rounds
reported `Progress: no`,
THE SYSTEM SHALL mark the Attempt as `needs-user`, write a durable
handoff naming the failed review artifacts and the consecutive
no-progress streak, and SHALL NOT plan another write round.
Untestable: Requires multiple consecutive review rounds with LLM reviewers all reporting Progress: no

WHEN the Attempt's total completed write Tasks reach
`FACTORY_MAX_TOTAL_WRITE_ROUNDS`,
THE SYSTEM SHALL mark the Attempt as `needs-user`, write the same
handoff form, and SHALL NOT plan another write round, even if no
consecutive no-progress streak has accumulated.
Test: tests/binary.rs (work_attempt_run_counts_already_planned_followup_against_budget)
Test: tests/binary.rs (work_attempt_run_plans_followup_for_failed_reviews)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop counts preplanned follow-up against budget)

WHEN no completed review artifact has a failing verdict and any completed
review artifact has an uncertain or missing verdict,
THE SYSTEM SHALL mark the Attempt as `needs-user`, mark the Attempt
review state as `uncertain`, and write a durable handoff that names the
uncertain or missing-verdict review artifacts.
Test: tests/binary.rs (work_attempt_run_marks_uncertain_reviews_needs_user)
Test: tests/binary.rs (work_attempt_run_marks_missing_verdict_needs_user)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop marks uncertain reviews needs-user)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop marks missing verdict needs-user)

IF a Task executor fails while `factory work attempt run` advances an
Attempt,
THEN THE SYSTEM SHALL leave the Work Item state written by the Task
executor intact and exit non-zero without planning later transitions.
Test: tests/binary.rs (work_attempt_run_stops_when_task_executor_fails)

IF `factory work attempt run <work-item-id> <attempt-id>` evaluates a
completed review Task whose stored `artifact_area.path` points outside
`.factory/work/artifacts/`,
THEN THE SYSTEM SHALL exit non-zero and leave stored Work Item state
unchanged.
Test: tests/binary.rs (work_attempt_run_rejects_unmanaged_completed_review_artifact_area_path)

IF `factory work attempt run <work-item-id> <attempt-id>` is invoked
for a missing Work Item, invalid Work Item id, missing Attempt, or
terminal Attempt,
THEN THE SYSTEM SHALL exit non-zero and leave stored Work Item state
unchanged.
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop invalid or terminal request leaves state unchanged)

IF a review Task coder exits successfully but does not write `review.md`,
THEN THE SYSTEM SHALL mark the Attempt and Task as `failed` and report
that the review artifact was not written.
Test: tests/binary.rs (work_task_run_fails_review_task_without_artifact)
Test: tests/binary.rs (work_task_run_ignores_stale_review_artifact)
Test: tests/behaviors/operations/test-work-task-run.sh (review Task missing artifact fails)

IF a review Task coder exits non-zero,
THEN THE SYSTEM SHALL mark the Attempt and Task as `failed`, leave Task
output unset, and report the coder failure.
Test: tests/binary.rs (work_task_run_marks_review_task_failed_when_coder_exits_nonzero)
Test: tests/behaviors/operations/test-work-task-run.sh (review coder failure marks Task failed)

IF a review Task mutates a readable candidate workspace,
THEN THE SYSTEM SHALL restore any committed candidate `HEAD` change, mark
the Attempt and Task as `failed`, and report the candidate workspace
mutation.
Test: tests/binary.rs (work_task_run_fails_review_task_that_dirties_candidate_workspace)
Test: tests/binary.rs (work_task_run_fails_review_task_that_dirties_candidate_workspace_and_exits_nonzero)
Test: tests/binary.rs (work_task_run_fails_review_task_that_commits_to_candidate_workspace)
Test: tests/binary.rs (work_task_run_restores_committed_review_mutation_before_dirty_failure)

IF the write Task coder exits non-zero,
THEN THE SYSTEM SHALL mark the Attempt and Task as `failed`, leave Task
output unset, and report the coder failure.
Test: tests/binary.rs (work_task_run_marks_task_failed_when_coder_exits_nonzero)
Test: tests/behaviors/operations/test-work-task-run.sh (coder failure marks Task failed)

IF the requested Task is missing, belongs to a different Attempt or Work
Item, is not planned, has an unsupported kind, has zero or multiple
writable workspaces for a write Task, declares a writable workspace
outside managed sibling workspaces, points to an existing directory that
is not a registered git worktree, or is a review Task that declares writable
workspaces, lacks an artifact area, declares no readable workspaces,
lacks review context, declares review context whose candidate is not a
readable workspace, declares an unmanaged artifact area path, or declares
an unmanaged readable workspace path,
THEN THE SYSTEM SHALL exit non-zero without creating an unmanaged
workspace or mutating Task completion state.
Test: tests/binary.rs (work_task_run_rejects_task_that_is_not_planned)
Test: tests/binary.rs (work_task_run_rejects_non_write_task)
Test: tests/binary.rs (work_task_run_requires_one_writable_workspace)
Test: tests/binary.rs (work_task_run_rejects_unmanaged_writable_workspace_path)
Test: tests/binary.rs (work_task_run_rejects_malformed_review_context)
Test: tests/binary.rs (work_task_run_rejects_existing_directory_that_is_not_worktree)
Test: tests/binary.rs (work_task_run_rejects_unmanaged_review_artifact_area_path)
Test: tests/binary.rs (work_task_run_rejects_unmanaged_review_read_workspace_path)
Test: tests/binary.rs (work_task_run_missing_ids_leave_work_item_unchanged)
Test: tests/behaviors/operations/test-work-task-run.sh (invalid task requests do not complete or mutate)

IF `factory work attempt <work-item-id> <attempt-id>` is invoked for a
missing Work Item, an invalid Work Item id, a duplicate Attempt id, or
an invalid Attempt id,
THEN THE SYSTEM SHALL exit non-zero and leave stored Work Item state
unchanged.
Test: tests/binary.rs (work_attempt_missing_work_item_reports_not_found)
Test: tests/binary.rs (work_attempt_duplicate_attempt_id_fails_without_changes)
Test: tests/binary.rs (work_attempt_rejects_invalid_attempt_id_without_changes)
Test: tests/behaviors/operations/test-work-attempt-intake-review.sh (missing Work Item does not create state)
Test: tests/behaviors/operations/test-work-attempt-intake-review.sh (duplicate Attempt id leaves item unchanged)
Test: tests/behaviors/operations/test-work-attempt-intake-review.sh (invalid ids leave Work Item state unchanged)

## Coder transient failures

WHEN a Coder request returns a rate-limit response (HTTP 429 or
provider equivalent) that includes a retry-after hint,
THE SYSTEM SHALL parse the hint into a `RateLimitInfo` carrying a
concrete `retry_at` instant and a human-readable reason string.
Test: src/coder.rs (rate_limit_parsing_tests::claude_code_parses_retry_after_seconds)
Test: src/coder.rs (rate_limit_parsing_tests::claude_code_parses_retry_after_ms)
Test: src/coder.rs (rate_limit_parsing_tests::claude_code_parses_reset_at_iso8601)
Test: src/coder.rs (rate_limit_parsing_tests::codex_parses_rate_limit_error_event)
Test: src/coder.rs (rate_limit_parsing_tests::fixture_claude_code_retry_after)
Test: src/coder.rs (rate_limit_parsing_tests::fixture_codex_retry_after)

WHEN the session loop encounters a parsed `RateLimitInfo`,
THE SYSTEM SHALL wait until `retry_at` plus a per-run randomized
jitter (uniform in `[0, JITTER_MAX_SECONDS]`, default 30) before
retrying.
Test: src/coder.rs (jitter_tests::jitter_respects_max)
Test: src/coder.rs (jitter_tests::jitter_returns_zero_when_max_is_zero)
Test: src/coder.rs (jitter_tests::jitter_respects_custom_max)

WHEN multiple concurrent Factory runs encounter rate limits with
the same `retry_at`,
THE SYSTEM SHALL apply each run's independent jitter so the runs
fan out instead of retrying at the same instant.
Test: src/coder.rs (jitter_tests::jitter_respects_max)
Test: src/coder.rs (jitter_tests::jitter_respects_custom_max)

IF a rate-limit response does not include a retry-after hint, or the
hint is unparseable,
THEN THE SYSTEM SHALL fall back to a conservative default wait
(`FACTORY_RATE_LIMIT_RETRY_AFTER_SECS`, default 1800) plus jitter —
matching previous behavior on this path.
Test: src/coder.rs (rate_limit_parsing_tests::claude_code_returns_none_for_no_timing)
Test: src/coder.rs (rate_limit_parsing_tests::claude_code_returns_none_for_unstructured_transcript)
Test: src/coder.rs (transcript_rate_limit_tests::detects_session_limit_marker)
Test: src/coder.rs (transcript_rate_limit_tests::detects_generic_rate_limit_phrase)
Test: src/coder.rs (transcript_rate_limit_tests::no_marker_returns_false)

WHEN a run transitions into rate-limit state (the first time the
session loop pauses for a parsed `retry_at` after a previously
non-rate-limited request),
THE SYSTEM SHALL fire a notification once via the existing macOS
`osascript` surface (or platform equivalent on non-macOS) stating
that the run paused and naming the expected resume time.
Test: src/coder.rs (rate_limit_state_tests::normal_to_rate_limited_fires_enter_notification)
Test: src/coder.rs (rate_limit_state_tests::full_cycle_fires_enter_once_and_leave_once)

WHEN a run transitions out of rate-limit state (the first successful
non-rate-limited request after a paused wait),
THE SYSTEM SHALL fire a notification once stating that the run
resumed.
Test: src/coder.rs (rate_limit_state_tests::full_cycle_fires_enter_once_and_leave_once)

WHEN the session loop retries within an ongoing rate-limit pause
(multiple retries against the same `retry_at` because the provider
returned another 429),
THE SYSTEM SHALL NOT fire additional enter-state notifications;
notifications fire on state transitions, not on each retry tick.
Test: src/coder.rs (rate_limit_state_tests::rate_limited_to_rate_limited_does_not_refire_notification)
Test: src/coder.rs (rate_limit_state_tests::full_cycle_fires_enter_once_and_leave_once)

WHEN the Coder abstraction is queried for rate-limit parsing,
THE SYSTEM SHALL provide a parser for the Anthropic provider and a
parser for the Codex provider, both returning
`Option<RateLimitInfo>` from a provider-specific response shape.
Test: src/coder.rs (rate_limit_parsing_tests::fixture_claude_code_retry_after)
Test: src/coder.rs (rate_limit_parsing_tests::fixture_codex_retry_after)
Test: src/coder.rs (rate_limit_parsing_tests::codex_parses_reset_at_iso8601)
Test: src/coder.rs (rate_limit_parsing_tests::fixture_codex_reset_at)

WHEN no transcript file is configured for a Coder invocation,
THE SYSTEM SHALL propagate the original exit code without rate-
limit retry, since transient failure cannot be detected without
the transcript content.
Untestable: Negative code path; all production callers configure a transcript

## Brief capture

WHEN the user invokes the capture-brief skill,
THE SYSTEM SHALL interview the user, research the codebase, and write
a brief for a Work Item, using `.factory/runs/[run-id]/brief.md` only
as legacy fallback or recovery state.
Test: tests/behaviors/skills/code-reviewer.md (test-skill)
Test: tests/behaviors/operations/test-planning-skills-work-context.sh

WHEN the user invokes the build-in-the-factory skill for new delegated
build work,
THE SYSTEM SHALL teach Work Items, Attempts, Tasks, Workspaces, and Merge
Candidates as the normal lifecycle, direct the user through Work Item
creation, Attempt execution, Merge Candidate inspection, and
`factory work merge`, and describe legacy `factory run` as compatibility,
Fargate-only, recovery, or explicit fallback.
Test: tests/behaviors/operations/test-build-in-factory-work-model-guidance.sh

WHEN the brief is confirmed by the user,
THE SYSTEM SHALL keep the approved brief available for later planning and
set legacy status to `briefed` only when using the legacy fallback.
Untestable: Requires interactive user confirmation in a conversation session

## Behavior definition

WHEN the user invokes the define-behaviors skill,
THE SYSTEM SHALL read the brief and existing behaviors, elaborate into
EARS-format behavioral statements, and write behaviors.diff.md.
Test: tests/behaviors/skills/run-summary-behaviors.md (test-skill)

WHEN behaviors are approved by the user,
THE SYSTEM SHALL keep the behavior diff available for Work Item planning
context and set legacy status to `behaviors-defined` only when using the
legacy fallback.
Test: tests/behaviors/skills/run-summary-behaviors.md (test-skill)

## Approach design

WHEN the user invokes the design-approach skill,
THE SYSTEM SHALL research external systems, evaluate options, and write
approach.md with relevant expertise references, key technical decisions,
and solution direction.
Untestable: Requires interactive LLM skill invocation with codebase research

WHEN the approach is approved by the user,
THE SYSTEM SHALL keep the approach available for Work Item planning
context and set legacy status to `approach-designed` only when using the
legacy fallback.
Untestable: Requires interactive user approval in a conversation session

## Execution planning

WHEN the user invokes the plan-execution skill,
THE SYSTEM SHALL break the approach into executable Work Item steps,
describe one Work Item with an Attempt and Task notes or peer Work Items
as the default planning units, and write plan.md.
Test: tests/behaviors/skills/format-check-plan.md (test-skill)
Test: tests/behaviors/operations/test-planning-skills-work-context.sh

WHEN the plan is approved by the user,
THE SYSTEM SHALL create the Work Item with approved planning context and
set legacy status to `planned` only when using the legacy fallback.
Test: tests/behaviors/operations/test-planning-skills-work-context.sh

WHEN the plan-execution skill describes parallel execution,
THE SYSTEM SHALL describe peer Work Items first, keep Attempt and Task
sequencing as planning notes rather than executable dependencies, and
label child-run decomposition as a legacy fallback.
Test: tests/behaviors/skills/parallel-work-items-plan.md (test-skill)
Test: tests/behaviors/operations/test-planning-skills-work-context.sh

## Legacy run worktree isolation

WHEN legacy `factory run` is invoked,
THE SYSTEM SHALL create a git worktree branched from the current HEAD,
copy the run's state into it, and execute within the worktree.
Test: src/worktree.rs (setup_run_worktree tests), tests/binary.rs (worktree creates and copies state)

WHEN legacy `factory run` is invoked from a non-main branch,
THE SYSTEM SHALL branch the worktree from that branch and record it as
the source-branch.
Test: tests/test-run (setup_run_worktree from non-main branch)

## Legacy session loop (local)

WHEN legacy `factory run` is invoked with the local runtime,
THE SYSTEM SHALL launch the selected coder in non-interactive mode with
the brief or handoff as the initial prompt.
Test: src/session.rs (test_loop_initial_prompt_uses_brief, test_loop_initial_prompt_uses_handoff), tests/binary.rs (run_uses_handoff_prompt_when_handoff_exists)

WHEN legacy `factory run --coder codex` is invoked with the local runtime,
THE SYSTEM SHALL launch Codex with `codex exec --json`, prepend the
factory system prompt to the run prompt, and capture Codex JSON output
as the session transcript.
Test: tests/binary.rs (run_with_codex_uses_exec_json_and_status_contract)

WHEN legacy `factory run` is invoked with an unknown coder,
THE SYSTEM SHALL fail before resolving or launching a run.
Test: tests/binary.rs (run_unknown_coder_fails)

WHEN the agent exits with status `executing`,
THE SYSTEM SHALL restart the agent.
Test: src/session.rs (test_loop_restarts_on_executing), tests/binary.rs (run_session_loop_restarts_on_executing)

WHEN the agent exits with status `needs-user`, `complete`, or `failed`,
THE SYSTEM SHALL stop the loop.
Test: src/session.rs (test_loop_stops_on_needs_user, test_loop_stops_on_failed), tests/binary.rs (run_session_loop_stops_on_complete, run_session_loop_stops_on_needs_user)

WHEN the agent exits with status `rate-limited`,
THE SYSTEM SHALL wait 5 minutes plus per-run jitter and restart the
agent.
Test: src/session.rs (test_loop_restarts_on_rate_limited)

IF the agent exits with a non-zero exit code 3 consecutive times,
THEN THE SYSTEM SHALL set status to `failed` and stop the loop.
Test: src/session.rs (test_loop_consecutive_failures_set_failed, test_loop_success_resets_failure_counter), tests/binary.rs (run_session_loop_consecutive_failures)

IF the session count exceeds 50,
THEN THE SYSTEM SHALL set status to `failed` and stop the loop.
Test: src/session.rs (test_loop_max_sessions_sets_failed)

## Legacy session observability

WHEN a session completes within the session loop,
THE SYSTEM SHALL write a line to `sessions.log` containing the session
number, exit code, duration, and status.
Test: src/session.rs (test_loop_writes_sessions_log, test_loop_writes_nonzero_exit_to_sessions_log), tests/binary.rs (run_writes_sessions_log), tests/behaviors/operations/test-observability.sh

WHEN the session loop launches an agent session,
THE SYSTEM SHALL request machine-readable JSON events from the selected
coder and pipe stdout to `sessions/session-N/transcript.jsonl`.
Test: src/session.rs (test_loop_creates_session_transcript_dir), tests/binary.rs (run_captures_stream_json_transcript), tests/behaviors/operations/test-observability.sh

## Legacy review archiving

WHEN a review round fails and a new round starts,
THE SYSTEM SHALL archive previous review artifacts to `reviews/round-N/`
before running new reviews. Review files and transcript files are moved,
leaving top-level `reviews/` artifacts for the current round only.
Test: src/review.rs (test_archive_previous_round_moves_reviews, test_archive_previous_round_noop_for_first_round), tests/binary.rs (run_archives_review_rounds), tests/behaviors/operations/test-observability.sh

WHEN a reviewer runs,
THE SYSTEM SHALL capture its stream-json output to
`reviews/transcript-{name}.jsonl`.
Test: tests/binary.rs (run_archives_review_rounds), tests/behaviors/operations/test-observability.sh

## Legacy session loop (local) — credential refresh

WHEN a new Claude session starts on the sandboxed local runtime,
THE SYSTEM SHALL run an unsandboxed Claude invocation to refresh the
OAuth token, then re-read the token from Keychain into the process
environment.
Test: src/session.rs (test_loop_calls_pre_session_before_each_session, test_loop_stops_when_pre_session_returns_error), tests/behaviors/operations/test-claude-runtime-hooks.sh (sandboxed claude runs refresh hook)

WHEN a new Codex session starts on the sandboxed local runtime,
THE SYSTEM SHALL NOT run the Claude credential refresh hook.
Test: tests/behaviors/operations/test-codex-runtime.sh (codex does not run claude refresh hook, parallel codex does not run claude refresh hook)

## Fargate teardown

WHEN `factory fargate teardown` is invoked,
THE SYSTEM SHALL delete the CloudFormation stack used by the Fargate
runtime, wait for stack deletion to reach a terminal state, and
report the deletion outcome.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)

WHEN `factory fargate teardown` is invoked without `--keep-ecr`,
THE SYSTEM SHALL delete the ECR repository created by the bootstrap.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)

WHEN `factory fargate teardown` is invoked without `--keep-s3`,
THE SYSTEM SHALL empty and delete the S3 bucket created by the
bootstrap.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)

WHEN `factory fargate teardown` is invoked with `--keep-ecr`,
THE SYSTEM SHALL leave the ECR repository intact while still deleting
the CloudFormation stack and the state file.
Test: tests/binary.rs (fargate_teardown_keep_ecr_skips_ecr_delete)

WHEN `factory fargate teardown` is invoked with `--keep-s3`,
THE SYSTEM SHALL leave the S3 bucket intact while still deleting the
CloudFormation stack and the state file.
Test: tests/binary.rs (fargate_teardown_keep_s3_skips_s3_delete)

WHEN `factory fargate teardown` completes its destructive steps,
THE SYSTEM SHALL delete `~/.config/factory/fargate.state.json` so the
next `--runtime fargate` invocation triggers a fresh `ensure_setup`.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)

IF `factory fargate teardown` is invoked when no state file exists
and no CloudFormation stack is present,
THEN THE SYSTEM SHALL exit zero with a message saying nothing
needed teardown.
Test: tests/binary.rs (fargate_teardown_nothing_to_teardown)

IF a destructive step fails (CloudFormation, ECR, or S3),
THEN THE SYSTEM SHALL print the error, exit non-zero, and leave the
state file in place so a retry resumes from the failed step.
Test: tests/binary.rs (fargate_teardown_error_preserves_state_file)

WHEN `factory fargate teardown` completes successfully,
THE SYSTEM SHALL print a one-line summary listing what was removed
and what was kept.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)
Test: src/fargate_bootstrap.rs (teardown_outcome_display_all_removed)
Test: src/fargate_bootstrap.rs (teardown_outcome_display_partial_keep_ecr)
Test: src/fargate_bootstrap.rs (teardown_outcome_display_partial_keep_s3)

## Legacy Fargate execution

WHEN legacy `factory run --runtime fargate` is invoked,
THE SYSTEM SHALL upload the worktree to S3, start an ECS Fargate task,
record `runtime=fargate`, and record the ECS task handle in the source
run directory.
Test: tests/binary.rs (run_fargate_launch_uploads_workspace_and_records_task_handle), tests/behaviors/operations/test-fargate-launch.sh

WHEN legacy `factory run --runtime fargate --coder codex` is invoked
and the host `~/.codex/auth.json` is missing,
THE SYSTEM SHALL fail with a clear error before launching the ECS task.
Test: tests/binary.rs (run_fargate_with_codex_fails_when_host_auth_missing)
Test: tests/behaviors/operations/test-codex-runtime.sh (fargate codex fails when host auth missing)

WHEN legacy `factory run --runtime fargate --coder codex` is invoked
and the host `~/.codex/auth.json` has `auth_mode != "chatgpt"`,
THE SYSTEM SHALL refuse to launch with a clear error stating that
Fargate Codex requires ChatGPT subscription auth.
Test: tests/binary.rs (run_fargate_with_codex_fails_when_host_auth_mode_is_apikey)
Test: tests/behaviors/operations/test-codex-runtime.sh (fargate codex fails when host auth mode is apikey)

WHEN the Fargate task starts,
THE SYSTEM SHALL pull the workspace from S3 and run the Rust session loop
in the downloaded workspace while preserving `runtime=fargate` and the
ECS task handle in the run directory.
Test: tests/binary.rs (run_in_place_can_preserve_run_metadata), tests/behaviors/operations/test-fargate-entrypoint.sh

WHEN the Fargate task reaches a terminal status,
THE SYSTEM SHALL upload the workspace to S3.
Test: tests/behaviors/operations/test-fargate-entrypoint.sh

## Fargate Codex support

WHEN `factory work attempt run <work-id> --runtime fargate --coder
codex` is invoked,
THE SYSTEM SHALL launch an ECS task using the Factory base image,
pass `FACTORY_CODER=codex` plus the Codex auth env var as task
overrides, and run the Attempt to completion producing the same
artifact shape that the local Codex path produces today.
Test: src/fargate.rs (codex_overrides_include_auth_json_and_factory_coder)

WHEN `factory work merge <work-id> --runtime fargate --coder codex`
is invoked,
THE SYSTEM SHALL launch an ECS task that runs the agentic rebase
step using Codex, then proceeds through merge checks, reviews, and
fast-forward unchanged from the Claude path.
Test: src/fargate.rs (codex_overrides_include_auth_json_and_factory_coder)

WHEN the Fargate entrypoint runs with `FACTORY_CODER=codex`,
THE SYSTEM SHALL require the `CODEX_AUTH_JSON` env var, write its
contents to `${HOME}/.codex/auth.json` with mode 0600 before
invoking the factory binary, and exit non-zero with a clear error
when the env var is missing.
Test: tests/behaviors/operations/test-fargate-entrypoint-codex.sh (codex writes auth.json and unsets OPENAI_API_KEY, codex missing CODEX_AUTH_JSON fails)

WHEN the Fargate entrypoint runs with `FACTORY_CODER=claude` or
`FACTORY_CODER` unset,
THE SYSTEM SHALL require the `CLAUDE_CODE_OAUTH_TOKEN` env var
(today's behavior, unchanged) and SHALL NOT require any Codex
credentials.
Test: tests/behaviors/operations/test-fargate-entrypoint-codex.sh (claude path unchanged, default coder is claude)

WHEN the Factory base image is built,
THE SYSTEM SHALL install both `@anthropic-ai/claude-code` and
`@openai/codex` via npm such that `claude` and `codex` are both on
the `PATH` of the runtime container user.
Test: infrastructure/run/Dockerfile (visual inspection)

WHEN ECS task launch occurs from a Fargate runtime path,
THE SYSTEM SHALL set `FACTORY_CODER` as a task override and pass the
appropriate auth env var (`CLAUDE_CODE_OAUTH_TOKEN` for Claude,
`CODEX_AUTH_JSON` for Codex) without logging, persisting, or echoing
the value into user-facing output.
Test: src/fargate.rs (claude_overrides_include_oauth_token_and_factory_coder, codex_overrides_include_auth_json_and_factory_coder)

WHEN the host machine launches a Fargate Codex run,
THE SYSTEM SHALL read the Codex auth from `~/.codex/auth.json` on
the host and pass its contents as the `CODEX_AUTH_JSON` task
override, returning a clear error when the host auth file is
missing or unreadable.
Test: src/fargate.rs (codex_overrides_err_when_host_auth_file_missing)

WHEN the host launches a Fargate Codex run and the host
`~/.codex/auth.json` has `auth_mode != "chatgpt"`,
THE SYSTEM SHALL refuse to launch with a clear error stating that
Fargate Codex requires ChatGPT subscription auth, and SHALL NOT
issue the ECS RunTask call.
Test: src/fargate.rs (codex_overrides_err_when_host_auth_mode_is_apikey)

WHEN the Fargate entrypoint runs with `FACTORY_CODER=codex`,
THE SYSTEM SHALL `unset OPENAI_API_KEY` in the entrypoint's
exported environment before invoking the factory binary, so any
accidental env-var leak cannot flip the run from ChatGPT subscription
billing to per-token API billing.
Test: tests/behaviors/operations/test-fargate-entrypoint-codex.sh (codex OPENAI_API_KEY unset in binary env)

IF the project's `.factory/Dockerfile` (or any base image layer)
ships a `~/.codex/config.toml` containing
`preferred_auth_method = "apikey"`,
THEN the Fargate Codex entrypoint SHALL detect that setting and
exit non-zero with an error explaining that Fargate Codex enforces
ChatGPT subscription billing.
Test: tests/behaviors/operations/test-fargate-entrypoint-codex.sh (codex config.toml apikey preference rejected)

IF the Factory base image is built without the Codex npm package,
THEN the Fargate Codex entrypoint check SHALL exit non-zero with
"codex binary not found on PATH" before any work is attempted.
Test: tests/behaviors/operations/test-fargate-entrypoint-codex.sh (codex missing CODEX_AUTH_JSON fails — env var check fires first; binary check fires after)

---

## Status reporting

WHEN Factory reads current run status for a run whose `worktree` file
points at an existing worktree containing `.factory/runs/[run-id]/`,
THE SYSTEM SHALL read status from that live worktree run directory before
falling back to the source run directory.
Test: tests/behaviors/operations/test-live-run-state.sh (current run status prefers live worktree), tests/binary.rs (status_prefers_live_worktree_status)

IF a run's `worktree` file is missing, empty, invalid, or points at a
worktree without `.factory/runs/[run-id]/`,
THEN THE SYSTEM SHALL continue to read current run artifacts from the
source run directory.
Test: tests/behaviors/operations/test-live-run-state.sh (invalid worktree falls back to source)

WHEN `factory status` is invoked,
THE SYSTEM SHALL display Work Item status by default and SHALL NOT
display legacy Run rows by default.
Test: tests/binary.rs (status_hides_runs_by_default), tests/behaviors/operations/test-work-status-dashboard.sh (status hides runs by default and prints Work summary)

WHEN `factory status --runs` is invoked,
THE SYSTEM SHALL display legacy Runs with their status, runtime, and
brief summary using the existing run summary format, and SHALL still
display Work Item status when Work Items or Work Item read errors exist.
Test: tests/binary.rs (status_runs_shows_runs_with_correct_fields, status_runs_shows_runs_and_work_items_together, status_prefers_live_worktree_status), tests/behaviors/operations/test-live-run-state.sh (status lists live status)

WHEN `factory status` is invoked and stored Work Items exist,
THE SYSTEM SHALL display a Work Items section with each Work Item's
latest Attempt, selected Task, review state, Merge Candidate, merge
state, actionable label, and title.
Test: tests/binary.rs (status_shows_work_items_without_runs, status_runs_shows_runs_and_work_items_together)

WHEN `factory status` lists an abandoned Work Item before cleanup,
THE SYSTEM SHALL surface it as terminal abandoned Work rather than as
Work that still needs human planning input.
Test: src/work_status.rs (summarize_abandoned_work_item_shows_terminal_action)
Test: tests/behaviors/operations/test-work-status-dashboard.sh (status surfaces abandoned Work as terminal)

WHEN `factory status` is invoked for a project with Work Items and no
legacy runs,
THE SYSTEM SHALL display the Work Items section instead of reporting
that no runs were found.
Test: tests/binary.rs (status_shows_work_items_without_runs)

WHEN `factory status` is invoked for a project with no Work Items and no
Work Item read errors,
THE SYSTEM SHALL report that no Work Items were found.
Test: tests/binary.rs (status_no_factory_dir, status_hides_runs_by_default)

WHEN `factory status` reads one or more invalid Work Item files,
THE SYSTEM SHALL report the invalid Work model path in a Work Item read
errors section while still displaying valid Work Items, and SHALL display
valid legacy Runs only when `--runs` is supplied.
Test: tests/binary.rs (status_reports_invalid_work_item_by_default_and_with_runs), tests/behaviors/operations/test-work-status-dashboard.sh (status reports invalid Work without hiding valid state)

WHEN `factory status` is invoked after cleanup,
THE SYSTEM SHALL hide cleaned legacy Runs unless `--runs` is supplied;
when `--runs` is supplied, THE SYSTEM SHALL list cleaned runs with their
existing run status and without a cleanup-specific status.
Test: tests/behaviors/operations/test-cleanup.sh (status lists cleaned runs with original status)

WHEN `factory status --runs` is invoked and a Fargate run exists,
THE SYSTEM SHALL display the locally recorded run status, runtime, and
brief summary without querying AWS.
Test: tests/behaviors/operations/test-status-edges.sh (status fargate uses local state without AWS)

## Legacy run summary

WHEN legacy `factory summary` is invoked,
THE SYSTEM SHALL summarize the active run using existing run artifacts
and print the summary to stdout.
Test: tests/binary.rs (summary_resolves_active_run)

WHEN legacy `factory summary --run-id <id>` is invoked,
THE SYSTEM SHALL summarize that run instead of resolving the active run.
Test: tests/binary.rs (summary_uses_explicit_run_id)

WHEN legacy `factory summary --run-id <id>` is invoked for a run with a
live worktree run directory,
THE SYSTEM SHALL prefer live status, sessions, review verdicts, handoff,
and report presence before falling back to source artifacts.
Test: tests/binary.rs (summary_prefers_live_worktree_artifacts), tests/behaviors/operations/test-live-run-state.sh (summary reads live artifacts)

WHEN a run summary is printed,
THE SYSTEM SHALL include the run's current phase derived from existing
status artifacts.
Test: tests/binary.rs (summary_resolves_active_run)

WHEN a run summary is printed,
THE SYSTEM SHALL include author activity from durable run artifacts.
Test: tests/binary.rs (summary_includes_sessions_reviews_handoff_and_report)

WHEN the summarized run has reviewer activity,
THE SYSTEM SHALL include reviewer activity from durable run artifacts.
Test: tests/binary.rs (summary_includes_sessions_reviews_handoff_and_report)

WHEN the summarized run has child runs,
THE SYSTEM SHALL include child run activity from durable run artifacts.
Test: tests/binary.rs (summary_includes_child_activity)

WHEN the summarized run has session history,
THE SYSTEM SHALL include the latest entries from `sessions.log`.
Test: tests/binary.rs (summary_includes_sessions_reviews_handoff_and_report)

WHEN the summarized run has review artifacts,
THE SYSTEM SHALL include reviewer verdicts grouped by reviewer name.
Test: tests/binary.rs (summary_includes_sessions_reviews_handoff_and_report)

WHEN the summarized run has `handoff.md`,
THE SYSTEM SHALL include the first actionable handoff or question
context.
Test: tests/binary.rs (summary_includes_sessions_reviews_handoff_and_report), tests/binary.rs (summary_prefers_explicit_handoff_question)

WHEN a run summary is printed,
THE SYSTEM SHALL include a status-derived next action.
Test: tests/binary.rs (summary_uses_explicit_run_id), tests/binary.rs (summary_prefers_explicit_handoff_question)

WHEN the summarized run has `report.md`,
THE SYSTEM SHALL show that a report is available without printing the
entire report.
Test: tests/binary.rs (summary_includes_sessions_reviews_handoff_and_report)

WHEN no run can be resolved for legacy `factory summary`,
THE SYSTEM SHALL fail with a clear error instead of printing an empty
summary.
Test: tests/binary.rs (summary_fails_without_resolved_run)

## Cleanup

WHEN `factory cleanup` is invoked,
THE SYSTEM SHALL scan the source `.factory/runs` registry and select
stale complete and merged runs by default.
Test: tests/binary.rs (cleanup_dry_run_reports_without_changes)

WHEN `factory cleanup --apply` cleans a run,
THE SYSTEM SHALL preserve the run directory and status while writing
cleanup context to `cleaned.md`.
Test: src/cleanup.rs (apply_writes_marker_without_status_change), tests/binary.rs (cleanup_apply_writes_marker_without_changing_status)

WHEN `factory cleanup --run-id <id>` targets an active, needs-user, or
failed run,
THE SYSTEM SHALL fail without writing cleanup artifacts.
Test: tests/binary.rs (cleanup_refuses_active_run, cleanup_refuses_failed_run)

WHEN cleanup sees a registered git worktree for a selected run,
THE SYSTEM SHALL remove that worktree through git worktree operations.
Test: tests/binary.rs (cleanup_apply_removes_registered_worktree)

WHEN cleanup runs without `--apply`,
THE SYSTEM SHALL report registered worktree removal without removing it.
Test: tests/binary.rs (cleanup_dry_run_keeps_registered_worktree)

WHEN cleanup sees a recorded worktree path that git does not register,
THE SYSTEM SHALL leave the path in place and report that it was skipped.
Test: src/cleanup.rs (unregistered_worktree_path_is_not_removed), tests/binary.rs (cleanup_skips_unregistered_worktree_path)

WHEN `factory cleanup` sees terminal Work Items,
THE SYSTEM SHALL report Work Item state, artifacts, managed candidate
worktrees, and Work branches without removing them unless `--apply` is
passed.
Test: tests/binary.rs (cleanup_work_items_dry_run_and_apply_manage_state_worktree_and_branch), tests/binary.rs (cleanup_work_items_removes_terminal_merge_candidate_artifacts_and_worktree)

WHEN `factory cleanup --apply` cleans a terminal Work Item,
THE SYSTEM SHALL remove the Work Item state, referenced managed Work
artifacts, registered managed candidate worktrees, and Work branches.
The cleanup SHALL also remove any Fargate runtime metadata directories
recorded under `.factory/work/runtime/attempts/<work-item-id>/` and
`.factory/work/runtime/merges/<work-item-id>/`.
Test: tests/binary.rs (cleanup_work_items_dry_run_and_apply_manage_state_worktree_and_branch), tests/binary.rs (cleanup_work_items_removes_terminal_merge_candidate_artifacts_and_worktree), src/cleanup.rs (terminal_work_item_cleanup_removes_runtime_arn_dirs)

WHEN `factory cleanup` sees an abandoned Work Item with no executing or
reviewing Attempts, no executing Tasks, no reviewing Merge Candidates,
and no executing Merge Candidate merges,
THE SYSTEM SHALL select it for cleanup, including its managed sibling
worktree, Work branch, state records, and Work artifacts.
Test: tests/behaviors/operations/test-cleanup.sh (cleanup selects abandoned needs-user Work Items)
Test: tests/behaviors/operations/test-cleanup.sh (cleanup skips abandoned Work with reviewing Attempt)
Test: tests/behaviors/operations/test-cleanup.sh (cleanup skips abandoned Work with active Merge Candidate)

WHEN Factory reads stored Work state with legacy artifact references
under `.factory/work/artifacts/<attempt-id>/...`,
THE SYSTEM SHALL expose those references under
`.factory/work/artifacts/<work-item-id>/<attempt-id>/...` and move
existing legacy artifacts into that namespace when no namespaced artifact
already exists.
Test: src/work_model.rs (store_migrates_legacy_work_artifact_paths_on_read)

WHEN Work cleanup sees artifact references that are absolute paths, use
parent escapes, or do not resolve under `.factory/work/artifacts/`,
THE SYSTEM SHALL ignore those unmanaged artifact references without
reporting or removing them.
Test: tests/behaviors/operations/test-cleanup.sh (Work cleanup ignores unmanaged artifacts)

WHEN Work cleanup sees active Work Items or Work Items with active Merge
Candidates,
THE SYSTEM SHALL leave them out of cleanup results.
Test: tests/binary.rs (cleanup_work_items_dry_run_and_apply_manage_state_worktree_and_branch), tests/binary.rs (cleanup_work_items_selects_failed_terminal_and_skips_pending_merge_candidate)

WHEN Work cleanup sees failed terminal Work Items,
THE SYSTEM SHALL select them for cleanup.
Test: tests/binary.rs (cleanup_work_items_selects_failed_terminal_and_skips_pending_merge_candidate)

WHEN `factory cleanup` sees a top-level
`.factory/work/artifacts/<work-item-id>/` directory without a matching
stored Work Item,
THE SYSTEM SHALL report the orphaned Work artifact root without removing
it unless `--apply` is passed.
Test: tests/binary.rs (cleanup_work_items_reports_and_removes_orphan_artifact_roots), tests/behaviors/operations/test-cleanup.sh (cleanup removes orphan Work artifact roots)

WHEN `factory cleanup --apply` sees a top-level
`.factory/work/artifacts/<work-item-id>/` directory without a matching
stored Work Item,
THE SYSTEM SHALL remove the orphaned Work artifact root and report that
it was removed.
Test: tests/binary.rs (cleanup_work_items_reports_and_removes_orphan_artifact_roots), tests/behaviors/operations/test-cleanup.sh (cleanup removes orphan Work artifact roots)

WHEN Work cleanup sees top-level entries under `.factory/work/artifacts/`
that are files or directories with matching stored Work Items,
THE SYSTEM SHALL ignore them for orphan Work artifact cleanup.
Test: tests/binary.rs (cleanup_work_items_reports_and_removes_orphan_artifact_roots), tests/behaviors/operations/test-cleanup.sh (cleanup removes orphan Work artifact roots)

WHEN the dashboard legacy Runs view selects a run,
THE SYSTEM SHALL prefer actionable runs over cleaned runs.
Test: src/dashboard.rs (test_app_new_prefers_actionable_run_over_cleaned_terminal_run)

## Workspace retrieval

WHEN `factory pull` is invoked,
THE SYSTEM SHALL download the completed workspace from S3 into the run's
worktree directory.
Test: tests/binary.rs (pull_downloads_workspace_to_recorded_worktree,
pull_downloads_workspace_to_fallback_target)

## Interactive access

WHEN `factory shell` is invoked,
THE SYSTEM SHALL open an interactive shell into the running Fargate
container via ECS Exec.
Test: tests/binary.rs (shell_opens_ecs_exec_for_recorded_task)

## Watch and notification

WHEN `factory watch` is invoked,
THE SYSTEM SHALL poll run status at the specified interval.
Test: tests/behaviors/operations/test-watch-and-status-edges.sh

WHEN a run's status changes to `complete`, `needs-user`, or `failed`,
THE SYSTEM SHALL fire a macOS notification containing the run ID, status,
and brief summary.
Test: tests/behaviors/operations/test-notification-content.sh

WHEN the status is `complete`,
THE NOTIFICATION SHALL include the session count and review verdict.
Test: tests/behaviors/operations/test-notification-content.sh

WHEN the status is `needs-user`,
THE NOTIFICATION SHALL include the first open question from handoff.md.
Test: tests/behaviors/operations/test-notification-content.sh

## Run-id resolution

WHEN a factory command needs the run-id,
THE SYSTEM SHALL check in order: `--run-id` flag, `FACTORY_RUN_ID` env
var, `.factory/active-run` file, then scan for active runs. The scan
considers a run active if its status is `planned`, `executing`, or `reviewing`.
Test: src/run.rs (resolve run-id tests), tests/binary.rs (run-id resolution tests)

## Review phase

WHEN the author sets status to `complete`,
THE SYSTEM SHALL set status to `reviewing`, run all reviewers in parallel,
and restore status to `complete` if all pass or `executing` if any fail,
unless the run qualifies for the no-change skip or still has dirty
worktree content after passing review.
Test: src/session.rs (review phase tests), tests/binary.rs (run_archives_review_rounds, run_reviews_when_complete_worktree_is_dirty)

WHEN all reviewers return verdict `pass`,
THE SYSTEM SHALL accept the run as complete and stop the loop when the
run worktree has no tracked changes, staged changes, or untracked
non-ignored files outside `.factory`.
Test: src/review.rs (verdict tests), src/session.rs (review phase tests)

WHEN any reviewer returns verdict `fail` or `uncertain`,
THE SYSTEM SHALL set status back to `executing` and restart the author
with the review findings.
Test: src/review.rs (test_extract_verdict_fail, test_extract_verdict_uncertain), tests/binary.rs (run_archives_review_rounds)

WHEN a review phase completes,
THE SYSTEM SHALL write `review-state.json` with the effective state,
round, source, and per-reviewer verdicts for that phase.
Test: src/review.rs (review-state tests), src/session.rs (review-only mode tests)

WHEN a reviewer prompt is missing, fails to launch, exits non-zero,
returns an error, panics, or fails to write its review artifact,
THE SYSTEM SHALL treat the reviewer result as non-passing and make the
review phase fail with a `reviews/review-[name].md` artifact that
records `Verdict: fail`.
Test: src/review.rs (reviewer execution failure tests)

## Legacy review runs

Full-codebase review-only work defaults to `factory work review-codebase`
and `factory work attempt run`. Legacy review runs remain available for
compatibility and recovery of existing `.factory/runs` state.

WHEN legacy `factory run` is invoked and the run's mode is `review`,
THE SYSTEM SHALL set status to `reviewing`, run reviewers with
full-codebase scope, and produce findings. No author session is launched.
Test: src/session.rs (review-only mode tests)

WHEN legacy `factory run` is invoked and the run has a `scope` file,
THE SYSTEM SHALL copy the scope file into the worktree.
Test: src/worktree.rs (test_worktree_copies_scope_file)

WHEN a review run completes its single review round and all reviewers
pass,
THE SYSTEM SHALL set status to `complete` and stop without launching
the author.
Test: src/session.rs (review-only mode tests)

WHEN a review run completes its single review round and any reviewer
does not pass,
THE SYSTEM SHALL set status to `failed` and stop without launching the
author.
Test: src/session.rs (review-only mode tests)

## Watch timeout

WHEN `factory watch --timeout N` is invoked,
THE SYSTEM SHALL stop polling after N seconds.
Test: tests/behaviors/operations/test-watch-timeout.sh (watch exits on timeout), tests/binary.rs (watch_exits_on_timeout)

## Skip reviews when no changes

WHEN the review phase triggers but the run has no committed changes, no
tracked worktree changes, no staged changes, no untracked non-ignored
files, and no explicit scope file was provided,
THE SYSTEM SHALL skip the review phase entirely and accept the run as
complete.
Test: tests/binary.rs (run_skips_reviews_when_no_code_changed)

WHEN the author sets status to `complete` and the run worktree has
tracked changes, staged changes, or untracked non-ignored files,
THE SYSTEM SHALL treat the run as changed and enter the review phase.
Test: src/session.rs (has_changes tests)

WHEN reviewers pass for a completed run but the run worktree still has
tracked changes, staged changes, or untracked non-ignored files outside
`.factory`,
THE SYSTEM SHALL write a handoff, set status to `executing`, and require
the author to make the worktree clean before final completion.
Test: tests/binary.rs (run_reviews_when_complete_worktree_is_dirty)

## Review round limit

IF the review-fix cycle has run 10 times,
THEN THE SYSTEM SHALL accept the current state, generate a report, and
complete the run when the worktree has no tracked changes, staged
changes, or untracked non-ignored files outside `.factory`.
Test: src/session.rs (complete_or_continue_dirty_completes_review_limit_clean_run, test_loop_review_limit_clean_worktree_records_acceptance)

IF the review-fix cycle has run 10 times and the worktree is clean,
THEN THE SYSTEM SHALL write `review-state.json` with state
`accepted-review-limit`, source `review-limit`, per-reviewer verdicts,
`max_rounds`, and a short reason.
Test: src/session.rs (test_loop_review_limit_clean_worktree_records_acceptance)

IF the review-fix cycle has run 10 times and the worktree is dirty,
THEN THE SYSTEM SHALL NOT write `accepted-review-limit`.
Test: src/session.rs (test_loop_review_limit_dirty_worktree_restarts_author)

## Effective review state

WHEN `factory merge` validates a complete run with `review-state.json`,
THE SYSTEM SHALL use that file as the effective review state before
consulting current review artifacts.
Test: src/run.rs (review-state tests), tests/binary.rs (run_merge_accepts_review_limit_state_with_stale_fail_artifact)

WHEN `factory merge` validates a complete run with review state `passed`
or `accepted-review-limit`,
THE SYSTEM SHALL treat the review state as accepted.
Test: src/run.rs (test_reviews_passed_prefers_review_state), tests/binary.rs (run_merge_accepts_review_limit_state_with_stale_fail_artifact)

WHEN `factory merge` validates a complete run with review state `failed`,
`uncertain`, or malformed JSON,
THE SYSTEM SHALL refuse to land.
Test: src/run.rs (test_reviews_passed_rejects_failed_review_state, test_reviews_passed_rejects_malformed_review_state)

WHEN `factory summary`, the generated run report, or the dashboard shows
a run with `review-state.json`,
THE SYSTEM SHALL use the recorded review state when presenting the run's
effective review outcome.
Test: src/summary.rs (summarize_prefers_review_state), src/report.rs (test_generate_report_prefers_review_state), src/dashboard.rs (test_run_view_review_state_summary_prefers_state_file)

## Parent death detection

WHILE `factory watch` is running, IF the parent process exits
(ppid changes),
THEN THE SYSTEM SHALL stop polling and exit.
Test: tests/behaviors/operations/test-watch-timeout.sh (watch detects parent exit)

## Resume

WHEN `factory resume` is invoked without a run ID and with a terminal on
stdin,
THE SYSTEM SHALL find a run with status `needs-user` or `failed` and
launch an interactive agent session for that run.
Test: tests/behaviors/operations/test-resume-resolve.sh, tests/binary.rs (resume_finds_live_needs_user_run)

WHEN `factory resume [RUN_ID]` is invoked with a terminal on stdin,
THE SYSTEM SHALL launch an interactive agent session for the named run.
Test: tests/behaviors/operations/test-resume-resolve.sh

WHEN `factory resume [RUN_ID]` is invoked without a terminal on stdin,
THE SYSTEM SHALL restart the selected run's session loop without
launching an interactive agent.
Test: tests/binary.rs (headless_resume_restarts_selected_run_loop), tests/behaviors/operations/test-headless-resume.sh

WHEN `factory resume [RUN_ID] --no-sandbox --coder codex` is invoked
without a terminal on stdin,
THE SYSTEM SHALL restart the selected run's session loop without
invoking `sandbox-exec`, and SHALL require the selected coder without
requiring the local Seatbelt runtime.
Test: tests/binary.rs (headless_resume_restarts_selected_run_loop, headless_resume_no_sandbox_does_not_require_sandbox_exec)

WHEN `factory --no-sandbox resume [RUN_ID] --coder codex` is invoked,
THE SYSTEM SHALL preserve the top-level no-sandbox behavior for resume.
Test: tests/binary.rs (headless_resume_global_no_sandbox_does_not_require_sandbox_exec),
tests/behaviors/operations/test-live-run-state.sh (resume uses live status rule)

WHEN both the top-level form (`factory --no-sandbox resume ...`) and
the local form (`factory resume ... --no-sandbox`) are present on the
same invocation,
THE SYSTEM SHALL honor the local form.
Test: tests/binary.rs (resume_local_coder_takes_precedence_over_global, headless_resume_global_no_sandbox_does_not_require_sandbox_exec)

WHEN neither `--no-sandbox` nor `--coder` is provided to `factory resume`,
THE SYSTEM SHALL apply the same defaults as `factory run` (sandbox
enabled, default coder).
Test: tests/binary.rs (resume_finds_needs_user_run)

WHEN `factory resume [RUN_ID] --no-sandbox` is invoked,
THE SYSTEM SHALL NOT pass `--no-sandbox` to the underlying coder as an
extra agent argument.
Test: tests/binary.rs (resume_local_no_sandbox_does_not_leak_into_extra_args)

WHEN `factory resume --help` is shown,
THE SYSTEM SHALL list the same local runtime flags as `factory run --help`,
including `--no-sandbox` and `--coder`.
Test: tests/binary.rs (resume_help_lists_local_runtime_flags)

WHEN `factory resume` is invoked without a run ID and without a terminal
on stdin,
THE SYSTEM SHALL find a run with status `needs-user` or `failed` and
restart that run's session loop without launching an interactive agent.
Test: tests/behaviors/operations/test-headless-resume.sh

WHEN `factory resume` selects or resumes a run,
THE SYSTEM SHALL use the live status rule to identify `needs-user` or
`failed` runs and to restart headless runs from the live worktree run
directory when it exists.
Test: tests/binary.rs (resume_finds_live_needs_user_run, headless_resume_restarts_selected_run_loop), tests/behaviors/operations/test-live-run-state.sh (resume uses live status rule)

WHEN headless `factory resume [RUN_ID]` targets a parallel parent run,
THE SYSTEM SHALL reject the resume without launching an agent.
Test: tests/binary.rs (headless_resume_rejects_parallel_parent), tests/behaviors/operations/test-headless-resume.sh

## Merge

WHEN `factory merge` is invoked and the run status is not `complete`,
THE SYSTEM SHALL refuse and exit non-zero.
Test: tests/behaviors/operations/test-run-merge.sh (land rejects non-complete run), tests/binary.rs (run_merge_rejects_non_complete_run)

WHEN `factory merge` is invoked for a run without `review-state.json` and
any current review artifact has verdict `fail`, `uncertain`, or is
missing a verdict line,
THE SYSTEM SHALL refuse and exit non-zero.
Test: tests/behaviors/operations/test-run-merge.sh (land rejects fail review verdict, land rejects uncertain review verdict), tests/binary.rs (run_merge_rejects_failed_reviews, run_merge_rejects_live_failed_reviews)

WHEN `factory merge [RUN_ID]` validates status and review artifacts before
merging,
THE SYSTEM SHALL prefer live worktree status and review artifacts before
falling back to source run artifacts.
Test: tests/binary.rs (run_merge_rejects_live_failed_reviews), tests/behaviors/operations/test-live-run-state.sh (land uses live status and reviews)

WHEN the project has no executable `.factory/hooks/check-pre-merge`,
THE SYSTEM SHALL run `factory merge` without requiring project checks.
Test: tests/binary.rs (run_merge_completes_full_lifecycle)

WHEN `.factory/hooks/check-pre-merge` exists and is executable,
THE SYSTEM SHALL run it in the run worktree before removing the
worktree, rebasing, merging, or marking the run merged, with Factory
context (`FACTORY_HOOK`, `FACTORY_ARTIFACT_DIR`, and any available
work/attempt/task identifiers) exposed as environment variables and
stdout+stderr captured to `<run_dir>/hooks/check-pre-merge.log`.
Test: tests/binary.rs (run_merge_runs_configured_check_before_merging)

WHEN `check-pre-merge` exits non-zero and no executable
`.factory/hooks/fix-pre-merge` is present,
THE SYSTEM SHALL exit non-zero, keep the worktree intact, keep the run
unlanded, and print the hook's exit code and the path to its captured
log file.
Test: tests/binary.rs (run_merge_runs_configured_check_before_merging)

WHEN `check-pre-merge` exits non-zero and an executable
`.factory/hooks/fix-pre-merge` is present,
THE SYSTEM SHALL require no uncommitted changes outside `.factory`
before running `fix-pre-merge`, run `fix-pre-merge` in the run worktree,
commit project changes outside `.factory` when the fix changes project
files, rerun reviewers after the autofix commit, rerun `check-pre-merge`,
and continue merging only if `fix-pre-merge` succeeds, the rerun
reviewers pass, and the recheck passes.
Test: tests/binary.rs (run_merge_refuses_autofix_when_worktree_has_user_changes, run_merge_autofixes_and_reruns_reviewers)

WHEN `fix-pre-merge` changes files and the subsequent reviewer rerun
fails or is uncertain,
THE SYSTEM SHALL keep the worktree intact, leave the run unlanded, copy
the new review artifacts to the source run directory, and exit non-zero.
Test: tests/binary.rs (run_merge_keeps_worktree_when_autofix_review_fails)

WHEN `factory merge` is invoked and the run worktree has tracked changes,
staged changes, or untracked non-ignored files outside `.factory`,
THE SYSTEM SHALL refuse and exit non-zero.
Test: tests/binary.rs (run_merge_rejects_dirty_completed_worktree)

WHEN `factory merge` completes successfully,
THE SYSTEM SHALL copy sessions/, sessions.log, reviews/, report.md, and
status from the worktree back to the source run directory.
Test: tests/behaviors/operations/test-run-merge.sh (land copies artifacts from worktree), tests/binary.rs (run_merge_completes_full_lifecycle)

WHEN `factory merge` completes successfully,
THE SYSTEM SHALL remove the worktree, rebase the run branch onto the
source branch, fast-forward merge into the source branch, and delete the
run branch.
Test: tests/behaviors/operations/test-run-merge.sh (land removes worktree, land deletes run branch, land merges run commits into main), tests/binary.rs (run_merge_completes_full_lifecycle, run_merge_preserves_linear_history)

WHEN `factory merge` completes successfully,
THE SYSTEM SHALL set the run status to `merged`.
Test: tests/binary.rs (run_merge_completes_full_lifecycle)

WHEN `factory merge` is invoked and the rebase has conflicts,
THE SYSTEM SHALL abort the rebase, exit non-zero, and leave the
repository in a clean state.
Test: tests/behaviors/operations/test-run-merge.sh (land fails on rebase conflict), tests/binary.rs (run_merge_fails_on_rebase_conflict)

WHEN `factory merge` is invoked without a run ID,
THE SYSTEM SHALL land the most recent complete run.
Test: tests/behaviors/operations/test-run-merge.sh, tests/binary.rs (run_merge_resolves_most_recent_complete_run)

## Dashboard

WHEN `factory dashboard` is invoked,
THE SYSTEM SHALL open the Work Items view by default.
Test: src/dashboard.rs (test_app_new_opens_work_view_with_legacy_runs_present), tests/behaviors/operations/test-work-status-dashboard.sh (dashboard shows Work Items alongside legacy runs)

WHEN `factory dashboard` is invoked and stored Work Items exist,
THE SYSTEM SHALL provide a Work Items view that shows Work Items,
latest Attempts, selected Tasks, review state, Merge Candidates, merge
state, and actionable labels.
Test: dashboard::tests::test_work_view_renders_work_items_without_runs,
tests/behaviors/operations/test-work-status-dashboard.sh (dashboard shows
Work Items alongside legacy runs)

WHEN legacy Runs exist,
THE SYSTEM SHALL let the user switch to the legacy Runs view from the
dashboard without making legacy Runs the default view.
Test: tests/behaviors/operations/test-work-status-dashboard.sh (dashboard shows Work Items alongside legacy runs)

WHEN the dashboard polls Work model state,
THE SYSTEM SHALL refresh the Work Items view from stored Work Item files
without requiring a dashboard restart.
Test: dashboard::tests::test_app_poll_refreshes_work_items, tests/behaviors/operations/test-work-status-dashboard.sh (dashboard refreshes Work Items on poll)

WHEN Work Items need user input, have pending Merge Candidates, or have
read errors, THE SYSTEM SHALL show top-level Work view counts for Work
Items, actionable rows, and errors.
Test: dashboard::tests::test_work_view_counts_errors,
tests/behaviors/operations/test-work-status-dashboard.sh (dashboard
surfaces actionable Work, dashboard reports Work read errors)

WHEN the Work Items view is selected and no Work Items exist,
THE SYSTEM SHALL show a Work empty state instead of the Runs empty
state.
Test: dashboard::tests::test_work_view_renders_empty_state_when_selected, tests/behaviors/operations/test-work-status-dashboard.sh (dashboard shows empty Work view)

WHEN `factory dashboard` is invoked for a project with no runs,
THE SYSTEM SHALL show an empty state instead of exiting with an error.
Test: tests/behaviors/operations/test-dashboard.sh (empty state instead of error with no runs), dashboard::tests::test_run_tabs_empty_no_panic

WHEN there are more runs than fit in the run tab bar,
THE SYSTEM SHALL keep the selected run visible and indicate
that more runs exist beyond the visible area.
Test: tests/behaviors/operations/test-dashboard.sh (no crash with many runs), dashboard::tests::test_run_tabs_overflow_shows_right_arrow, dashboard::tests::test_run_tabs_selected_always_visible, dashboard::tests::test_clamp_run_tab_offset_keeps_selected_visible

WHEN the dashboard renders run tabs,
THE SYSTEM SHALL show each run's status from its live run directory
when available.
Test: dashboard::tests::test_run_tabs_show_cached_live_status

WHEN `factory dashboard` chooses an initial run without `--run-id`,
THE SYSTEM SHALL prefer runs whose live status is `executing` or
`planned`.
Test: dashboard::tests::test_app_new_selects_run_with_live_active_status

WHEN the dashboard polls run state,
THE SYSTEM SHALL remove runs whose source run directories no longer
exist.
Test: dashboard::tests::test_app_poll_removes_deleted_runs_and_selects_existing_run

IF the selected run is removed during dashboard polling,
THEN THE SYSTEM SHALL select an existing run when one remains.
Test: dashboard::tests::test_app_poll_removes_deleted_runs_and_selects_existing_run

IF all runs are removed during dashboard polling,
THEN THE SYSTEM SHALL render the empty-state dashboard.
Test: dashboard::tests::test_app_poll_renders_empty_state_after_all_runs_removed

WHEN `factory dashboard --run-id` is invoked with a non-existent run ID,
THE SYSTEM SHALL exit gracefully without crashing.
Test: tests/behaviors/operations/test-dashboard.sh (dashboard handles invalid run-id)

WHEN `factory dashboard` is invoked,
THE SYSTEM SHALL not modify any run state files.
Test: tests/behaviors/operations/test-dashboard.sh (dashboard does not modify run state)

WHEN the dashboard displays a run with status `complete` or `merged`
and that run has `report.md`,
THE SYSTEM SHALL show the run report in the activity feed by default.
Test: dashboard::tests::test_completed_run_with_report_shows_report_by_default

WHEN the dashboard displays a completed run without `report.md`,
THE SYSTEM SHALL continue to show the available transcript activity.
Test: dashboard::tests::test_completed_run_without_report_shows_author_transcript

WHEN the dashboard displays a run whose status is not `complete` or
`merged`,
THE SYSTEM SHALL continue to show live transcript activity by default,
even when `report.md` exists.
Test: dashboard::tests::test_nonterminal_run_with_report_shows_author_transcript

WHEN a completed run has `report.md` and author or reviewer transcripts,
THE SYSTEM SHALL keep the transcript views accessible from the dashboard.
Test: dashboard::tests::test_report_view_keeps_transcript_tabs_accessible

WHEN a run completes after the user has selected a transcript tab,
THE SYSTEM SHALL keep that transcript tab selected instead of switching
to the report view during dashboard polling.
Test: dashboard::tests::test_completion_poll_keeps_touched_transcript_selection

WHEN a run is in the review phase,
THE SYSTEM SHALL show each reviewer as an agent tab displaying a status
symbol and color: ✓ (Green) for pass, ✗ (Red) for fail, ? (Yellow) for
uncertain, ⟳ (Cyan) for running.
Untestable: Requires live TUI rendering with review-phase run state and colored symbols

WHEN a new review round starts,
THE SYSTEM SHALL derive current reviewer tabs and verdicts only from
top-level `reviews/transcript-*.jsonl` and `reviews/review-*.md`
artifacts. Archived `reviews/round-N/` artifacts shall not create
reviewer tabs or current verdicts.
Test: dashboard::tests::test_discover_agents_resets_archived_review_round_verdicts, tests/behaviors/operations/test-dashboard-review-rounds.sh (archived reviews do not drive current verdict, archived transcripts do not create current tabs)

WHILE a run is actively executing (author or reviewers running),
THE SYSTEM SHALL show a visual indicator that distinguishes "active"
from "idle" at a glance in the selected-run header, dashboard title,
agent tabs, and run tabs.
Test: dashboard::tests::test_header_spinner_advances_with_tick, dashboard::tests::test_dashboard_title_shows_global_activity, dashboard::tests::test_run_view_has_activity_from_status, dashboard::tests::test_run_view_has_activity_from_running_reviewer, dashboard::tests::test_agent_tab_running_shows_spinner_symbol, dashboard::tests::test_header_author_executing_shows_spinner, dashboard::tests::test_run_tabs_show_active_status_marker, dashboard::tests::test_run_tabs_active_status_marker_advances, tests/behaviors/operations/test-dashboard-activity.sh (no crash when run is actively executing, no crash when reviewers are running)

WHEN the dashboard polls run state,
THE SYSTEM SHALL keep actionable runs sorted before terminal runs, keep
cleaned terminal runs last, and preserve the selected run by run ID.
Test: dashboard::tests::test_app_poll_sorts_actionable_runs_first, dashboard::tests::test_app_new_prefers_actionable_run_over_cleaned_terminal_run

WHEN everything is done (no processes running, terminal status),
THE SYSTEM SHALL stop showing the spinner animation in the header and
display the final status without activity indicators.
Test: dashboard::tests::test_header_complete_no_spinner, dashboard::tests::test_header_failed_no_spinner, tests/behaviors/operations/test-dashboard-activity.sh (no crash when run is complete)

WHEN a reviewer finishes,
THE SYSTEM SHALL reflect the new verdict immediately.
Test: dashboard::tests::test_agent_tab_shows_verdict_immediately, dashboard::tests::test_discover_agents_updates_verdict, tests/behaviors/operations/test-dashboard-activity.sh (no crash when reviewer verdict arrives)

WHEN the dashboard legacy Runs view displays a run,
THE SYSTEM SHALL show a phase label that accurately describes what is
happening right now (executing, reviewing, complete, failed, needs input,
rate-limited, planned). The `reviewing` phase shows a spinner, including
before reviewer transcripts exist.
Test: dashboard::tests::test_header_reviewing_shows_progress, dashboard::tests::test_header_complete_no_spinner, dashboard::tests::test_header_failed_no_spinner, dashboard::tests::test_compute_phase_needs_user, dashboard::tests::test_compute_phase_rate_limited, dashboard::tests::test_header_rate_limited_shows_spinner, dashboard::tests::test_compute_phase_planned, tests/behaviors/operations/test-dashboard-activity.sh (no crash when run has failed, no crash when run needs user input, no crash with mixed run states), tests/behaviors/operations/test-dashboard-review-rounds.sh (reviewing status shows active work before transcripts)

### Dashboard layout

WHEN the dashboard Work Items view is displayed,
THE SYSTEM SHALL render four vertical regions: Work Items counts header,
view tabs, Work Items list, and help bar.
Test: dashboard::tests::test_work_view_counts_errors,
dashboard::tests::test_work_view_renders_work_items_without_runs,
dashboard::tests::test_work_view_renders_empty_state_when_selected

WHEN the dashboard legacy Runs view is displayed,
THE SYSTEM SHALL render five vertical regions: header (run ID, status,
session count, event count), run tabs, agent tabs, activity feed, and
help bar.
Untestable: Requires live TUI rendering of five-region vertical layout

### Dashboard keyboard navigation

WHEN the user presses `q` or Ctrl+C,
THE SYSTEM SHALL exit the dashboard and restore the terminal.
Untestable: Requires live terminal restore after process exit

WHEN the user presses Tab,
THE SYSTEM SHALL select the next agent tab within the current run,
display that tab's transcript, report, or review artifact content in the
activity feed, reset the scroll position, and re-enable auto-scroll.
Untestable: Requires live TUI keyboard dispatch with visual feed update and scroll reset

WHEN the user presses Shift-Tab,
THE SYSTEM SHALL select the previous agent tab within the current run,
display that tab's transcript, report, or review artifact content in the
activity feed, reset the scroll position, and re-enable auto-scroll.
Untestable: Requires live TUI keyboard dispatch with visual feed update and scroll reset

WHEN the user presses ← or →,
THE SYSTEM SHALL select the previous or next run and display that run's
activity feed. Each run preserves its own scroll position and auto-scroll
state independently.
Untestable: Requires live TUI with multiple runs and per-run scroll state preservation

WHEN the user presses j, k, ↑, or ↓,
THE SYSTEM SHALL scroll the activity feed by one line and disable
auto-scroll. If scrolling down reaches the bottom, auto-scroll
re-enables.
Test: dashboard::tests::test_scroll_down_reenables_auto_scroll_at_bottom

WHEN the user presses G or End,
THE SYSTEM SHALL scroll the activity feed to the bottom and re-enable
auto-scroll.
Test: dashboard::tests::test_scroll_to_bottom_enables_auto_scroll

WHEN the user presses g or Home,
THE SYSTEM SHALL scroll the activity feed to the top and disable
auto-scroll.
Untestable: Requires live TUI keyboard dispatch with scroll position and auto-scroll toggle

WHEN the user presses PgUp or PgDn,
THE SYSTEM SHALL scroll the activity feed by 20 lines. If PgDn reaches
the bottom, auto-scroll re-enables.
Test: dashboard::tests::test_page_down_reenables_auto_scroll_at_bottom

### Dashboard mouse scroll

WHEN the user scrolls the mouse wheel up,
THE SYSTEM SHALL scroll the activity feed up by 3 lines and disable
auto-scroll.
Untestable: Requires live TUI mouse event capture with scroll position tracking

WHEN the user scrolls the mouse wheel down,
THE SYSTEM SHALL scroll the activity feed down by 3 lines. If the scroll
position reaches the bottom, auto-scroll re-enables.
Test: dashboard::tests::test_mouse_scroll_down_reenables_auto_scroll_at_bottom

### Dashboard copy mode

WHEN the user presses `c`,
THE SYSTEM SHALL toggle copy mode: disable mouse capture so the terminal
allows text selection, and show a [COPY MODE] indicator in the help bar.
Pressing `c` again re-enables mouse capture.
Test: dashboard::tests::test_toggle_copy_mode, dashboard::tests::test_help_bar_shows_copy_key, dashboard::tests::test_help_bar_shows_copy_mode_indicator

### Dashboard activity feed

WHEN a line in the activity feed exceeds the terminal width,
THE SYSTEM SHALL wrap the line at display-column boundaries with a
2-space continuation indent, preserving all characters.
Test: dashboard::tests::test_activity_feed_wrapping_no_cutoff, dashboard::tests::test_activity_feed_wrapping_continuation_not_truncated, dashboard::tests::test_activity_feed_multibyte_wrapping

WHEN the activity feed contains content with ANSI escape sequences,
THE SYSTEM SHALL strip all ANSI CSI and OSC sequences before rendering,
preserving the visible text that follows escape terminators.
Test: dashboard::tests::test_strip_ansi_csi_terminator_preserves_next_char, dashboard::tests::test_activity_feed_ansi_multibyte_no_stray_chars

WHILE auto-scroll is enabled,
THE SYSTEM SHALL keep the bottom of the feed visible as new events arrive.
Untestable: Requires live TUI feed rendering with streaming event arrival

### Dashboard render and poll cadence

WHILE the dashboard is running,
THE SYSTEM SHALL render frames at ~75ms intervals for smooth animation
and poll for new data at ~2s intervals to avoid unnecessary I/O.
Untestable: Requires timing measurement of render and poll intervals in live TUI

## Parallel plan execution

### Plan parsing

WHEN a run has a plan.md with structured parallel groups,
THE SYSTEM SHALL create a child run for each step before execution begins.
Test: tests/behaviors/operations/test-parallel-runs.sh (parallel plan creates child runs)

WHEN a plan has no parallel groups (single sequential list),
THE SYSTEM SHALL execute normally as a single session loop.
Test: tests/behaviors/operations/test-parallel-runs.sh (single-step plan uses serial loop, no plan uses serial loop)

### Parallel execution

WHEN a parallel group is ready to execute,
THE SYSTEM SHALL launch all child runs in that group concurrently, each
in its own worktree.
Test: tests/behaviors/operations/test-parallel-runs.sh (parallel plan creates child runs, child failure preserves sibling worktrees)

WHILE child runs are executing,
THE SYSTEM SHALL show their status in the dashboard.
Test: tests/behaviors/operations/test-parallel-runs.sh (child runs shown in dashboard without crash)

### Merging

WHEN all child runs in a parallel group complete successfully,
THE SYSTEM SHALL run configured pre-merge checks for each child run and
merge their changes into the parent branch before launching the next
group.
Test: tests/behaviors/operations/test-parallel-runs.sh (sequential groups run in order), src/parallel.rs (test_parallel_children_run_pre_land_checks)

### Failure handling

WHEN a child run fails,
THE SYSTEM SHALL stop execution, report which step failed, and leave
sibling runs' worktrees intact for inspection.
Test: tests/behaviors/operations/test-parallel-runs.sh (child failure marks parent failed, child failure preserves sibling worktrees)

### Sequential gating

WHEN a plan has multiple groups,
THE SYSTEM SHALL execute them in order, launching each group only after
the previous group's merge completes.
Test: tests/behaviors/operations/test-parallel-runs.sh (sequential groups run in order)

## Legacy sandbox compatibility (local)

WHILE running Claude on the sandboxed local runtime,
THE SYSTEM SHALL execute Claude inside a macOS Seatbelt sandbox with
filesystem write access restricted to the run worktree and the source
repository's common git directory.
Test: tests/behaviors/operations/test-sandbox.sh (dry-run renders profile with workspace root, sandbox enforces filesystem boundary, sandbox blocks write outside workspace, sandboxed run uses sandbox-exec, sandboxed run can commit and blocks sibling write)

WHEN the sandbox behavior suite starts on a host where `sandbox-exec`
exists but cannot apply a minimal Seatbelt profile,
THE SUITE SHALL fail with an explicit message that sandbox behavior
coverage requires a working Seatbelt runtime.
Test: tests/behaviors/operations/test-sandbox-prereq.sh

WHEN legacy `factory run --coder codex` is invoked with the sandboxed
local runtime,
THE SYSTEM SHALL launch Codex under `sandbox-exec` with Factory's
Seatbelt profile, approval policy `never`, and the run worktree as
`--cd`, while disabling Codex's own sandbox. The rendered profile SHALL
include `common.sb` plus the Codex-specific `codex.sb` layer. The
Codex process SHALL receive `SSL_CERT_FILE` for a file-based CA bundle
selected by Factory, even when the caller inherited a different
`SSL_CERT_FILE`.
Test: tests/behaviors/operations/test-codex-runtime.sh (sandboxed codex uses factory seatbelt, sandboxed codex prefers factory ca bundle), tests/behaviors/operations/test-codex-approval-flag.sh (approval-policy flag appears before exec)

WHEN legacy `factory run --coder codex --no-sandbox` is invoked,
THE SYSTEM SHALL launch Codex with
`--dangerously-bypass-approvals-and-sandbox`.
Test: tests/binary.rs (run_with_codex_uses_exec_json_and_status_contract)

WHILE running inside the sandbox,
THE SYSTEM SHALL inject credentials via environment variables, never by
opening filesystem access to credential stores.
Test: tests/behaviors/operations/test-sandbox.sh (profile denies Keychain Mach services, profile denies credential filesystem access, credentials injected via env vars)

## Per-project Fargate images

WHEN `factory fargate ensure-setup` runs and `.factory/Dockerfile`
does not exist in the project root,
THE SYSTEM SHALL create it as a stub containing `ARG FACTORY_BASE_URI`
and `FROM ${FACTORY_BASE_URI}` (plus a brief comment on how to extend
it), and SHALL leave the file uncommitted for the user to inspect and
version-control.
Test: src/fargate_bootstrap.rs (ensure_project_dockerfile_stub_creates_when_missing, ensure_project_dockerfile_stub_skips_when_exists)
Test: tests/binary.rs (fargate_ensure_setup_creates_dockerfile_stub_when_missing)

WHEN `factory fargate ensure-setup` runs,
THE SYSTEM SHALL build the Factory base image and push it to the
project's ECR repo tagged with the current Factory version (e.g.,
`factory-base-0.1.0`), unless an image with that tag already exists
in the repo, in which case the build is skipped.
Test: src/fargate_bootstrap.rs (base_image_tag_includes_version)
Test: tests/binary.rs (fargate_ensure_setup_skips_base_build_when_ecr_tag_exists)

WHEN `factory fargate ensure-setup` runs,
THE SYSTEM SHALL compute the SHA-256 of the project's
`.factory/Dockerfile`, check the project's ECR repo for the project
image tagged with that hash (e.g., `project-a3f2b8c9d4e1`), build and
push the project image if missing, and skip the build if present.
Test: src/fargate_bootstrap.rs (project_image_tag_from_hash_deterministic_12_hex, sha256_file_is_stable, sha256_file_changes_with_content)
Test: tests/binary.rs (fargate_ensure_setup_skips_project_build_when_ecr_tag_exists)

WHEN `factory work merge --runtime fargate` runs,
THE SYSTEM SHALL launch the ECS task using the project image whose
tag matches the SHA-256 of `.factory/Dockerfile` at launch time.
Untestable: Requires live ECS task launch with ECR project image

WHEN `factory work merge --runtime fargate` runs and the project
image for the current `.factory/Dockerfile` content hash does not
exist in ECR,
THE SYSTEM SHALL build and push the project image (same procedure
as bootstrap) before launching the ECS task.
Untestable: Requires ECR image absence detection and Docker build during ECS launch

WHEN a local Attempt or local post-merge review runs (no `--runtime
fargate`),
THE SYSTEM SHALL NOT consult `.factory/Dockerfile` and SHALL NOT
build or launch any container; the user's local environment is used
as today.
Untestable: Negative behavior; local execution is the default path tested by all non-Fargate tests

WHEN `factory fargate teardown` runs,
THE SYSTEM SHALL delete both the Factory base image tags and the
project image tags from the project's ECR repo, in addition to the
existing teardown behaviors.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)

WHEN this repo's `.factory/Dockerfile` is used to build the project
image,
THE SYSTEM SHALL produce an image that contains `rustc`, `cargo`,
`rustfmt`, and `clippy` such that `cargo fmt --check`, `cargo test`,
and `cargo clippy` execute successfully under the merge-check hook.
Untestable: Requires Docker image build and Rust toolchain verification inside container

IF the project's `.factory/Dockerfile` cannot be built (syntax
error, unreachable base image, network failure during `docker
build`),
THEN `factory fargate ensure-setup` and `factory work merge
--runtime fargate` SHALL exit non-zero with a clear error that
names the failing build step and leaves the project's ECR repo
unchanged.
Untestable: Requires Docker build failure during Fargate bootstrap or merge

WHEN the project's `.factory/Dockerfile` references a `FROM
<ecr-uri>/factory-base:<version>` tag that does not exist in ECR,
THE SYSTEM SHALL surface the missing-tag error from `docker build`
to the user without retry, so the user can either bump Factory to
match the referenced base or update the `FROM` line.
Untestable: Requires Docker build with missing ECR base image tag

## Non-interactive git defaults

WHEN Factory invokes git through the wrapper module,
THE SYSTEM SHALL set the environment variables `GIT_EDITOR=true`,
`GIT_SEQUENCE_EDITOR=true`, and `GIT_TERMINAL_PROMPT=0` on the git
subprocess.
Test: src/git.rs (build_command_sets_non_interactive_env)

WHEN Factory invokes git through the wrapper module,
THE SYSTEM SHALL pass `-c commit.gpgsign=false` and
`-c core.editor=true` to every git subcommand.
Test: src/git.rs (build_command_passes_gpgsign_false, build_command_passes_core_editor_true)

WHEN any Factory code path issues a commit in a candidate worktree
(writer Task, agentic rebase, fix-pre-merge auto-commit, or any
future commit-producing path),
THE SYSTEM SHALL produce an unsigned commit regardless of the
project's global or repo-level git config.
Test: src/git.rs (build_command_passes_gpgsign_false)

WHEN the git wrapper is invoked,
THE SYSTEM SHALL capture stdout and stderr and surface non-zero exit
codes with the full command line plus captured output, so a failure
is debuggable without re-running.
Test: src/git.rs (run_returns_error_with_full_context)

IF a git operation genuinely requires user interaction (merge
conflict not resolved by the caller, credential prompt, signing
secret unlock, etc.),
THEN THE SYSTEM SHALL exit non-zero with diagnostic context and
SHALL NOT silently block the agent.
Test: src/git.rs (run_returns_error_with_full_context)

WHEN this Work Item lands,
THE SYSTEM SHALL contain zero direct `Command::new("git")` call sites
in `src/` outside the wrapper module.
Test: tests/binary.rs (no_direct_git_command_in_src)

## Per-task timestamps

WHEN Factory creates a new Task (write, review, or rebase),
THE SYSTEM SHALL set `task.created_at` to the current UTC time in
ISO 8601 / RFC 3339 format before persisting the Task JSON.
Test: src/work_model.rs (initial_attempt_populates_created_at_timestamps)

WHEN Factory transitions a Task out of Planned for the first time,
THE SYSTEM SHALL set `task.started_at` to the current UTC time if
it is not already set.
Test: src/work_model.rs (mark_task_started_is_idempotent)

WHEN Factory transitions a Task to a terminal status (Complete,
Failed, or NeedsUser),
THE SYSTEM SHALL set `task.completed_at` to the current UTC time.
Test: src/work_model.rs (set_task_terminal_sets_completed_at_and_status,
set_task_terminal_is_idempotent_on_completed_at)

WHEN Factory creates a new Attempt,
THE SYSTEM SHALL set `attempt.created_at` to the current UTC time
before persisting the Attempt JSON.
Test: src/work_model.rs (initial_attempt_populates_created_at_timestamps)

WHEN Factory transitions an Attempt to a terminal status (Complete,
Failed, or NeedsUser),
THE SYSTEM SHALL set `attempt.completed_at` to the current UTC time.
Test: src/work_model.rs (set_attempt_terminal_round_trip)

WHEN Factory creates a new MergeCandidate,
THE SYSTEM SHALL set `merge_candidate.created_at` to the current
UTC time before persisting the Merge Candidate JSON.
Test: src/work_model.rs (merge_candidate_creation_populates_created_at)

WHEN Factory transitions a MergeCandidate's merge_state.status out
of Pending for the first time (typically to Executing),
THE SYSTEM SHALL set `merge_candidate.started_at` to the current
UTC time if it is not already set.
Test: src/work_model.rs (mark_merge_candidate_started_is_idempotent)

WHEN Factory transitions a MergeCandidate to a terminal status
(Merged, Failed, or NeedsUser),
THE SYSTEM SHALL set `merge_candidate.completed_at` to the current
UTC time.
Test: src/work_model.rs (set_merge_candidate_terminal_round_trip)

WHEN `factory work show` returns Task, Attempt, and Merge Candidate
JSON,
THE SYSTEM SHALL include the timestamp fields verbatim from their
on-disk representation.
Test: src/work_model.rs (task_with_timestamps_round_trips,
attempt_round_trips_with_timestamps,
merge_candidate_round_trips_with_timestamps)

WHEN Factory reads a pre-existing Task, Attempt, or Merge Candidate
JSON file that does not contain the new timestamp fields,
THE SYSTEM SHALL deserialize cleanly with None values for the
missing fields and SHALL NOT fail the read or attempt any backfill.
Test: src/work_model.rs (legacy_json_without_timestamp_fields_deserializes_to_none)

WHEN Factory writes a Task, Attempt, or Merge Candidate JSON file,
THE SYSTEM SHALL skip emitting timestamp keys whose value is None,
so on-disk JSON stays compact and existing JSON files remain stable
when re-saved unchanged.
Test: src/work_model.rs (task_default_serializes_without_timestamps)

## Per-test log output

WHEN a Rust binary test in `tests/binary.rs` runs through the
`LoggedCommand` harness wrapper,
THE SYSTEM SHALL write the test's full captured stdout and stderr to
`tests/output/<test-name>.log`, truncating any prior content from a
previous run.
Test: tests/binary.rs (log_command_writes_log_file_on_success)

WHEN a shell behavior test function in `tests/behaviors/` invokes the
shared `run_test` helper,
THE SYSTEM SHALL write the function's full captured stdout and stderr to
`tests/output/<test-file-name>/<case-name>.log`, truncating any prior
content from a previous run.
Test: tests/behaviors/lib/test-log-harness.sh

WHEN any test writes its captured output to the durable log file,
THE SYSTEM SHALL also pass the output through to the user's terminal
unchanged so interactive runs see exactly what they would see without
logging.
Test: tests/behaviors/lib/test-log-harness.sh

WHEN the test harness writes per-test log files,
THE SYSTEM SHALL create `tests/output/` and any required subdirectories
on demand and SHALL NOT fail the test on a log-write error; instead it
surfaces a one-line warning and continues.
Test: tests/lib/log.rs (write_log error handling)

WHEN `FACTORY_TESTS_SKIP_LOG=1` is set in the test process environment,
THE SYSTEM SHALL bypass per-test log-writing entirely — no file created,
no warning printed.
Test: tests/binary.rs (log_command_skips_on_factory_tests_skip_log)
Test: tests/behaviors/lib/test-log-harness.sh

WHEN any case fails during the test run,
THE SYSTEM SHALL print a "Failing case logs:" section at the end of the
run listing each failed case's absolute log path and the last 20 lines
of that log inline.
Test: tests/behaviors/lib/test-log-harness.sh
Test: tests/binary.rs (log_command_failed_command_appends_to_failed_sentinel)

WHEN the test harness lists per-test log paths in the failed-test
summary,
THE SYSTEM SHALL print absolute paths so clipboard-copy-paste into
another shell works without `cd` context.
Test: tests/behaviors/lib/test-log-harness.sh

WHEN `tests/output/` is present in the repository,
THE SYSTEM SHALL gitignore it so per-run logs never appear in
`git status`, never get committed, and never appear in diffs.
Test: .gitignore (tests/output/ entry)

## Behavior tests Task

WHEN Factory plans review Tasks for a completed Attempt,
THE SYSTEM SHALL include one `TaskKind::BehaviorTests` Task with
id `<attempt-id>-behavior-tests` alongside the review Tasks when
the behaviors reviewer role is included.
Test: src/work_model.rs (review_tasks_include_behavior_tests_task_when_behaviors_role_present)

WHEN the behaviors reviewer role is absent from the review plan,
THE SYSTEM SHALL skip creating a BehaviorTests Task.
Test: src/work_model.rs (review_tasks_skip_behavior_tests_when_behaviors_role_absent)

WHEN a `TaskKind::BehaviorTests` Task is executed,
THE SYSTEM SHALL launch an LLM agent that reads
`documentation/behaviors.md` from the candidate workspace, runs each
`RunBehaviorTests:` command, parses structured output, maps each
`Test:` reference to its outcome, and writes
`behavior-tests-results.json` to the Task's artifact directory.
Untestable: Requires live LLM agent execution environment

WHEN `behavior-tests-results.json` is written,
THE SYSTEM SHALL produce per-behavior entries with `anchor`,
`test_refs`, `status` (pass, fail, untestable, missing_test_ref),
`duration_ms` when known, and `failure_excerpt` when status is fail.
Test: src/behavior_tests.rs (behavior_tests_results_round_trip)

WHEN the candidate's `behaviors.md` contains an EARS statement with
an `Untestable:` marker,
THE SYSTEM SHALL record that behavior as `status: untestable` in the
results JSON with `untestable_reason` set to the marker's text.
Test: src/behavior_tests.rs (behavior_tests_results_round_trip)

WHEN the candidate's `behaviors.md` contains an EARS statement with
no `Test:` reference and no `Untestable:` marker,
THE SYSTEM SHALL record that behavior as `status: missing_test_ref`.
Test: src/behavior_tests.rs (behavior_status_serializes_lowercase)

WHEN Factory schedules the review-phase Tasks of an Attempt,
THE SYSTEM SHALL start `behavior-tests` in parallel with the other
reviewers, and SHALL block the behaviors-completeness reviewer until
`behavior-tests` completes.
Test: src/work_model.rs (review_tasks_include_behavior_tests_task_when_behaviors_role_present)
Test: src/work_attempt_loop.rs (tasks_ready_to_run_skips_dependents_until_dependency_complete)
Test: src/work_attempt_loop.rs (tasks_ready_to_run_returns_dependent_after_dependency_completes)

WHEN the behaviors-completeness reviewer runs,
THE SYSTEM SHALL read `behavior-tests-results.json` and the candidate's
behavior increment, verify that every new or changed EARS statement has
either a `Test:` reference or an `Untestable:` marker, verify that
every `Test:` reference's entry has `status: pass`, and produce a
review.md artifact with a verdict and findings.
Untestable: Reviewer behavior is LLM-driven and verified by the skill definition

IF the `behavior-tests` Task's LLM agent cannot resolve the
`RunBehaviorTests:` commands,
THEN the agent SHALL write `behavior-tests-results.json` with a
top-level `command_failure` field and an empty `behaviors` array.
Test: src/behavior_tests.rs (command_failure_results_round_trip)

WHEN the behaviors-completeness reviewer reads a results JSON
containing `command_failure`,
THE SYSTEM SHALL produce a `fail` verdict naming the failed command.
Untestable: Reviewer behavior is LLM-driven and verified by the skill definition

WHEN `TaskKind::BehaviorTests` is serialized and deserialized,
THE SYSTEM SHALL round-trip as `"behavior-tests"`.
Test: src/work_model.rs (task_kind_behavior_tests_round_trips)

WHEN a Task has a `depends_on` field referencing another Task,
THE SYSTEM SHALL skip that Task in the ready-to-run check until the
dependency completes.
Test: src/work_model.rs (task_with_depends_on_round_trips)
Test: src/work_attempt_loop.rs (tasks_ready_to_run_skips_dependents_until_dependency_complete)
Test: src/work_attempt_loop.rs (tasks_ready_to_run_returns_dependent_after_dependency_completes)

WHEN a Task has no `depends_on` field,
THE SYSTEM SHALL consider it immediately ready to run.
Test: src/work_attempt_loop.rs (tasks_ready_to_run_returns_independent_tasks_immediately)
Test: src/work_model.rs (task_without_depends_on_omits_field)

## Auto-merge watcher

WHEN `factory work auto-merge <work-item-id>` is invoked,
THE SYSTEM SHALL poll the named Work Item's state every 30 seconds
and fire `factory work merge <work-item-id> <merge-candidate-id>`
on the Work Item's latest Attempt's Merge Candidate when its
`review_state == passed` and `merge_state.status == pending` and
`merge_state.auto_merge_skipped` is not `true`.
Test: src/auto_merge.rs (find_ready_candidate_returns_some_when_attempt_passed_and_candidate_pending)
Test: tests/binary.rs (auto_merge_exits_clean_on_sigterm)

WHEN `factory work auto-merge --all` is invoked,
THE SYSTEM SHALL poll every Work Item in the project every 30
seconds and fire merge on any Merge Candidate that satisfies the
ready conditions above. Each WI is evaluated independently each
tick.
Test: src/auto_merge.rs (find_ready_candidate_returns_some_when_attempt_passed_and_candidate_pending)

WHEN `factory work auto-merge` is invoked with both `<work-item-id>`
and `--all` set,
THE SYSTEM SHALL exit non-zero with a clear error stating the two
modes are mutually exclusive.
Test: tests/binary.rs (auto_merge_with_both_flags_set_errors)

WHEN the watcher fires a merge that succeeds,
THE SYSTEM SHALL print
`[auto-merge] merged <work-item-id> at <commit-sha>` to stderr.
Test: src/auto_merge.rs (classify_merge_outcome_succeeds_on_ok)

WHEN the watcher fires a merge that fails for a reason other than
authentication,
THE SYSTEM SHALL set `merge_state.auto_merge_skipped = true` on
the Merge Candidate, persist it via the store, print
`[auto-merge] skipping <work-item-id> (merge failed: <one-line
reason>)` to stderr, and continue polling other Work Items.
Test: src/auto_merge.rs (classify_merge_outcome_treats_other_errors_as_failed)
Test: src/work_model.rs (mark_merge_candidate_auto_merge_skipped_round_trips)

WHEN the watcher fires a merge that fails with an authentication
error (e.g., the agentic rebase agent returns 401),
THE SYSTEM SHALL print
`[auto-merge] authentication expired, pausing <work-item-id>` to
stderr, leave `auto_merge_skipped` unset, and continue polling.
Test: src/auto_merge.rs (classify_merge_outcome_recognizes_401_as_auth_expired)
Test: src/auto_merge.rs (classify_merge_outcome_recognizes_invalid_authentication_phrase)
Test: src/auto_merge.rs (classify_merge_outcome_recognizes_authentication_failed)

WHEN a future tick observes a Merge Candidate with
`merge_state.auto_merge_skipped == true`,
THE SYSTEM SHALL skip that candidate and SHALL NOT attempt to
merge it, even if its `review_state` and `merge_state.status`
otherwise satisfy the ready conditions.
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_auto_merge_skipped)
Test: tests/binary.rs (auto_merge_skips_candidate_already_marked_skipped)

WHEN a future tick observes a Merge Candidate whose
`merge_state.status == needs-user` or whose
`review_state != passed`,
THE SYSTEM SHALL skip the candidate without setting
`auto_merge_skipped` and SHALL pick it up automatically on a
later tick if it transitions back to a ready state.
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_merge_status_needs_user)
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_candidate_review_not_passed)

WHEN `factory work auto-merge` receives SIGINT or SIGTERM while
no merge is in progress,
THE SYSTEM SHALL exit zero immediately.
Test: tests/binary.rs (auto_merge_exits_clean_on_sigterm)

WHEN `factory work auto-merge` receives SIGINT or SIGTERM while a
merge is in progress,
THE SYSTEM SHALL allow the current merge invocation to complete
(success, failure, or hang on a long agentic rebase), persist any
resulting state changes via the store, then exit.
Untestable: Requires a long-running merge mock; verified by code inspection of the polling loop

WHEN the watcher's polling tick reads Work Item state from
`.factory/work/`,
THE SYSTEM SHALL NOT modify any state files itself; state mutation
happens only through `factory work merge` invocations and the
store APIs those invocations call.
Test: src/auto_merge.rs (find_ready_candidate_returns_some_when_attempt_passed_and_candidate_pending)

WHEN the watcher sees a Work Item with no Attempts or no Merge
Candidates yet,
THE SYSTEM SHALL skip it on this tick without printing anything
and re-evaluate on the next tick.
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_no_attempts)

WHEN `factory work auto-merge` is invoked with neither a Work Item
ID nor `--all`,
THE SYSTEM SHALL exit non-zero with a clear error.
Test: tests/binary.rs (auto_merge_with_neither_flag_set_errors)

WHEN `factory work auto-merge` runs and a Merge Candidate
satisfies the ready conditions on multiple consecutive ticks,
THE SYSTEM SHALL fire merge only once and then either mark the
candidate skipped (on failure) or wait for the candidate to
transition to `merged` (on success); subsequent ticks observe
the new state and do not re-fire.
Untestable: Emergent from merge success transitioning status to `merged` and merge failure setting `auto_merge_skipped`, both of which prevent re-fire on the next tick

WHEN `MergeCandidateMergeState` is deserialized from JSON that
does not contain `auto_merge_skipped`,
THE SYSTEM SHALL default the field to `None` (backward-compatible).
Test: src/work_model.rs (legacy_merge_state_json_deserializes_with_none_skipped)

WHEN `MergeCandidateMergeState` is serialized with
`auto_merge_skipped == None`,
THE SYSTEM SHALL omit the field from the JSON output.
Test: src/work_model.rs (merge_state_skips_serializing_auto_merge_skipped_when_none)

## Claude auth token expiry detection

WHEN any Claude coder variant (`SandboxedClaudeCode`,
`BareClaudeCode`) is about to launch the `claude` process,
THE SYSTEM SHALL call `claude_auth::ensure_not_expired()` first,
and SHALL bail with the error's user-facing message (the Task fails
with `TaskStatus::Failed`) if the call returns an error.
Untestable: Structural integration verified by code inspection — `ensure_not_expired()` is the first call in both `SandboxedClaudeCode::run` and `BareClaudeCode::run`

WHEN `claude_auth::ensure_not_expired()` is invoked,
THE SYSTEM SHALL read the keychain entry under service
`Claude Code-credentials`, parse the `claudeAiOauth` object, and
return `Ok(())` if the access token's `expiresAt` is more than 5
minutes in the future.
Test: src/claude_auth.rs (tests::check_token_expiry_returns_ok_when_more_than_5min_remaining)
Test: src/claude_auth.rs (tests::keychain_envelope_deserializes_with_refresh_token)

WHEN `claude_auth::ensure_not_expired()` finds the access token
within 5 minutes of `expiresAt` (or already expired),
THE SYSTEM SHALL return `AuthError::Expired { expires_at }` with
the parsed expiry, so callers can produce a user-facing message
naming the recovery action (`claude /login`).
Test: src/claude_auth.rs (tests::check_token_expiry_returns_expired_within_margin)
Test: src/claude_auth.rs (tests::check_token_expiry_returns_expired_when_already_expired)
Test: src/claude_auth.rs (tests::check_token_expiry_boundary_at_exactly_5min)

WHEN `claude_auth::ensure_not_expired()` cannot read the keychain
entry (missing entry, malformed JSON, decode error),
THE SYSTEM SHALL return `Ok(())` rather than an error, on the
assumption that the user is on an API-key path that doesn't use
the keychain. The coder runs as today; any genuine auth failure
surfaces through the recovery layer.
Test: src/claude_auth.rs (tests::check_token_expiry_returns_ok_when_no_creds)
Test: src/claude_auth.rs (tests::keychain_envelope_deserializes_without_claude_ai_oauth)

WHEN the keychain entry's `claudeAiOauth.refreshToken` field is
absent or null,
THE SYSTEM SHALL return `Ok(())` (treating the session as API-key
only) and skip the expiry check.
Test: src/claude_auth.rs (tests::check_token_expiry_returns_ok_when_no_refresh_token)
Test: src/claude_auth.rs (tests::keychain_envelope_deserializes_without_refresh_token)

WHEN `src/coder.rs::run_with_transcript_retrying` observes the
coder process exit non-zero AND the transcript's most recent
`result` event has `api_error_status == 401`,
THE SYSTEM SHALL return `AuthError::Rejected { request_id }`
(populated from the transcript's `result.request_id` when
present) so the caller bails with a recovery message (the Task
fails with `TaskStatus::Failed`).
Test: src/claude_auth.rs (tests::classify_transcript_401_returns_rejected_on_result_401)
Test: src/claude_auth.rs (tests::classify_transcript_401_extracts_request_id_when_present)
Test: src/claude_auth.rs (tests::classify_transcript_401_returns_rejected_with_none_request_id_when_missing)

WHEN the recovery layer's 401 detection fires alongside the
existing rate-limit detection on the same attempt,
THE SYSTEM SHALL prefer the 401 surface (the auth issue is the
proximate cause; the rate-limit envelope may be incidental).
Untestable: Structural ordering verified by code inspection — `classify_transcript_401` is called before `parse_rate_limit_from_transcript` in `run_with_transcript_retrying`

WHEN the user-facing error message is constructed for either
`AuthError::Expired` or `AuthError::Rejected`,
THE SYSTEM SHALL name the recovery action explicitly, mentioning
`claude /login` and `retry the Task` in the message.
Test: src/claude_auth.rs (tests::auth_error_expired_user_message_names_login_action)
Test: src/claude_auth.rs (tests::auth_error_rejected_user_message_names_login_action)

WHEN any Codex coder variant (`CodexCode`) is about to launch,
THE SYSTEM SHALL NOT call `claude_auth::ensure_not_expired()`.
Codex auth lifecycle is out of scope for this Work Item.
Untestable: Structural absence verified by code inspection — `CodexCode::run` does not reference `claude_auth`

## Keep-awake toggle

WHEN `factory keep-awake on` is invoked on macOS,
THE SYSTEM SHALL ensure a single `caffeinate -i` process managed by
a Factory wrapper script is running, and print
`keep-awake on (caffeinate PID <pid>)` to stdout.
Test: tests/behaviors/operations/test-keep-awake.sh (on first invocation installs LaunchAgent and wrapper script)

WHEN `factory keep-awake on` is invoked and a Factory-managed
caffeinate process is already running,
THE SYSTEM SHALL print `keep-awake already on (caffeinate PID
<pid>)` to stdout and exit zero without spawning a duplicate.
Test: tests/behaviors/operations/test-keep-awake.sh (on when already running prints already-on with PID)

WHEN `factory keep-awake on` is invoked for the first time (no
LaunchAgent plist installed),
THE SYSTEM SHALL write the wrapper script to
`~/.config/factory/keep-awake-caffeinate`, write
`~/Library/LaunchAgents/com.factory.keep-awake.plist`, invoke
`launchctl bootstrap gui/$UID <plist>`, and print one additional
line confirming the LaunchAgent installation.
Test: tests/behaviors/operations/test-keep-awake.sh (on first invocation installs LaunchAgent and wrapper script)

WHEN `factory keep-awake on` succeeds,
THE SYSTEM SHALL enable the LaunchAgent's RunAtLoad and KeepAlive
policies so the user's intended-on state persists across reboots.
Test: src/keep_awake.rs (plist_round_trips_keepalive_true)

WHEN `factory keep-awake off` is invoked while a Factory-managed
caffeinate process is running,
THE SYSTEM SHALL invoke `launchctl bootout` to unload the
LaunchAgent, rewrite the plist with KeepAlive and RunAtLoad
disabled, wait for the process to exit, and print
`keep-awake off` to stdout.
Test: tests/behaviors/operations/test-keep-awake.sh (off when running calls bootout and updates plist)

WHEN `factory keep-awake off` is invoked and no caffeinate process
is running,
THE SYSTEM SHALL print `keep-awake already off` to stdout, and
exit zero.
Test: tests/behaviors/operations/test-keep-awake.sh (off when not running prints already-off)

WHEN `factory keep-awake status` is invoked,
THE SYSTEM SHALL print `on (caffeinate PID <pid>)` if a
Factory-managed caffeinate wrapper process is discoverable via
`pgrep -f <sentinel>`, or `off` otherwise, to stdout.
Test: tests/behaviors/operations/test-keep-awake.sh (status reports off when no caffeinate process is running)
Test: tests/behaviors/operations/test-keep-awake.sh (status reports on with caffeinate PID when process is running)

WHEN the macOS user logs in or the laptop boots,
THE SYSTEM SHALL respect the last toggle state: if `off` was the
most recent state, the LaunchAgent's disabled RunAtLoad and
KeepAlive policies keep caffeinate from running; if `on` was the
most recent state, launchctl starts caffeinate via the LaunchAgent.
Untestable: Requires macOS login/reboot cycle

WHEN `factory keep-awake uninstall` is invoked,
THE SYSTEM SHALL send `SIGTERM` to the running caffeinate process
if any, invoke `launchctl bootout` against the LaunchAgent,
delete the plist file and wrapper script, and print a one-line
confirmation.
Test: tests/behaviors/operations/test-keep-awake.sh (uninstall removes plist and wrapper script)
Test: tests/behaviors/operations/test-keep-awake.sh (uninstall when already uninstalled prints already-uninstalled)

WHEN any `factory keep-awake` subcommand is invoked on a non-macOS
platform,
THE SYSTEM SHALL exit non-zero with a clear error stating the
subcommand is macOS-only, and SHALL NOT touch the LaunchAgent or
any other state.
Test: src/keep_awake.rs (ensure_macos_errors_on_non_macos)

WHEN the LaunchAgent spawns caffeinate at boot or login,
THE SYSTEM SHALL use the Factory wrapper script at
`~/.config/factory/keep-awake-caffeinate` as the ProgramArguments
target so subsequent `status`, `off`, and `on` invocations
correctly observe the running process via `pgrep -f` against the
wrapper path.
Test: src/keep_awake.rs (plist_contains_valid_xml_structure)

---

## Git lock-error retry

WHEN `git::run`, `git::run_stdout`, or `git::run_raw` invokes git
and the invocation exits non-zero with stderr matching a known git
lock-error pattern,
THE SYSTEM SHALL sleep with exponential backoff (starting at 20ms,
doubling each attempt, capping at 320ms after the 5th retry) plus
±25% random jitter, and re-invoke the same command up to a total
of 8 attempts before giving up.
Test: tests/binary.rs (git_wrapper_succeeds_after_config_lock_clears_within_budget)
Test: tests/binary.rs (git_wrapper_succeeds_after_index_lock_clears_within_budget)
Test: tests/binary.rs (git_wrapper_bails_when_lock_persists_past_budget)

WHEN the wrapper detects a lock error,
THE SYSTEM SHALL recognize the following stderr patterns as
lock-class errors: `could not lock`, `lock failed`, `: File exists`
(with the path indicating a `.lock` suffix or known lock file),
and `Resource temporarily unavailable` against a `.lock` path.
Test: src/git.rs (is_lock_error_recognizes_could_not_lock_config)
Test: src/git.rs (is_lock_error_recognizes_index_lock_file_exists)
Test: src/git.rs (is_lock_error_recognizes_head_lock_resource_temporarily_unavailable)
Test: src/git.rs (is_lock_error_recognizes_refs_lock_file_exists)
Test: src/git.rs (is_lock_error_recognizes_lock_failed)

WHEN a retried git invocation succeeds within the 8-attempt budget,
THE SYSTEM SHALL return `Ok(())` / `Ok(stdout)` / `Ok(Output)` to
the caller as if the invocation had succeeded on the first try,
with no log output and no indication that retries occurred.
Test: tests/binary.rs (git_wrapper_succeeds_after_config_lock_clears_within_budget)
Test: tests/binary.rs (git_wrapper_succeeds_after_index_lock_clears_within_budget)

WHEN the 8-attempt retry budget is exhausted,
THE SYSTEM SHALL emit one stderr line naming the failed command,
the number of attempts made, and the underlying lock-error
stderr, then return the same error type the wrapper produces for
any other non-zero exit.
Test: tests/binary.rs (git_wrapper_bails_when_lock_persists_past_budget)

WHEN the git invocation exits non-zero with stderr that does NOT
match a lock-error pattern,
THE SYSTEM SHALL return the error immediately without retrying.
Test: tests/binary.rs (git_wrapper_does_not_retry_on_non_lock_error)
Test: src/git.rs (is_lock_error_does_not_match_authentication_failure)
Test: src/git.rs (is_lock_error_does_not_match_network_error)
Test: src/git.rs (is_lock_error_does_not_match_unrelated_file_exists_error)

WHEN the git invocation exits zero on the first attempt,
THE SYSTEM SHALL return success without any sleep or retry
overhead.
Test: tests/binary.rs (git_wrapper_succeeds_on_first_attempt_when_no_lock_error)

WHEN the wrapper's backoff sleep would push wall-clock past the
total budget (~1.5s),
THE SYSTEM SHALL apply the final sleep anyway and then make the
last attempt before giving up — the budget is approximate, not
strict.
Test: tests/binary.rs (git_wrapper_bails_when_lock_persists_past_budget)
Test: src/git.rs (backoff_duration_caps_at_320ms_after_5th_attempt)

WHEN multiple concurrent Factory processes each retry the same
git lock,
THE SYSTEM SHALL apply per-process jitter so the retries do not
collide on identical sleep intervals (thundering-herd avoidance).
Test: src/git.rs (lock_jitter_factor_within_range)
Test: src/git.rs (backoff_duration_applies_jitter_within_25_percent)
