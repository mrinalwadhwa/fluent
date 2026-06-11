# Behaviors

Observable behaviors of the factory system. Each statement describes what
the system does, not how. EARS format.

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
artifact directory; SHALL tell the reviewer to put build caches, scratch
files, suggested patches, and temporary outputs under the reviewer
artifact directory when the candidate workspace is read-only; SHALL
include Cargo guidance to set `CARGO_TARGET_DIR` under the reviewer
artifact directory for tests against a read-only workspace; and SHALL
NOT instruct the reviewer to write legacy
`.factory/runs/<run-id>/reviews/...` artifacts.
Test: src/work_task_executor.rs (work_review_prompt_names_work_artifacts_and_writable_outputs)

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

WHEN a normal Work Attempt completes a follow-up write Task created from
failed review artifacts,
THE SYSTEM SHALL plan the next review round only for the failed reviewer
roles that fed that follow-up write Task.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_mixed_failed_and_uncertain_reviews)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop plans follow-up with mixed missing review)

WHEN a normal Work Attempt completes a follow-up write Task created from
failed review artifacts and Factory plans a targeted follow-up review
round,
THE SYSTEM SHALL attach the relevant prior failed review artifact for
each planned review Task role as that review Task's input artifact.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_mixed_failed_and_uncertain_reviews)

IF a normal Work Attempt completes a follow-up write Task and the failed
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

WHEN Factory executes merge-time Work reviewers for a Merge Candidate,
THE SYSTEM SHALL create each reviewer's worktree at
`../review-<work-item-id-bytelen>-<work-item-id>-<attempt-id>-<reviewer>`
relative to the project root, run reviewer roles in parallel, and give
each reviewer only its own writable merge review artifact directory.
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge reviewers run in parallel)

WHEN parallel merge-time Work reviewers finish,
THE SYSTEM SHALL aggregate reviewer verdicts in the configured reviewer
order, write one combined merge review state, and land only when all
required reviewer verdicts pass.
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge lands after update, checks, and reviewers)

IF a parallel merge-time Work reviewer fails, returns a failing verdict,
does not write `review.md`, or dirties its reviewer worktree,
THEN THE SYSTEM SHALL fail the Merge Candidate without changing the
target branch.
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge failed reviewer leaves target unchanged)
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge missing reviewer artifact leaves target unchanged)
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge dirty reviewer leaves target unchanged)

WHEN Factory uses merge reviewer worktrees,
THE SYSTEM SHALL remove those reviewer worktrees after successful merge
or failed review handling while preserving durable review artifacts
under `.factory/work/artifacts/`.
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge reviewers run in parallel)

WHEN Factory launches a merge-time reviewer,
THE SYSTEM SHALL set `CARGO_TARGET_DIR` in the reviewer's environment to
a path inside that reviewer's artifact directory under
`.factory/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/reviews/<reviewer>/`,
so Cargo writes build outputs there rather than into the reviewer's
worktree or the candidate workspace.
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge lands after update, checks, and reviewers)

WHEN Factory launches a merge-time reviewer for a Work Item,
THE SYSTEM SHALL grant the reviewer process read access to the whole
`.factory/work/artifacts/<work-item-id>/<attempt-id>/` subtree through
the sandbox profile, so referenced merge-check and prior-review
artifact paths are readable.
Test: src/work_merge_executor.rs (merge_review_readable_sandbox_roots_includes_attempt_artifact_subtree)

WHEN Factory generates a merge-time reviewer prompt,
THE SYSTEM SHALL NOT instruct the reviewer to copy the candidate
workspace to `/tmp`, export `CARGO_TARGET_DIR`, or otherwise manage
build cache redirection.
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge lands after update, checks, and reviewers)

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
SHALL NOT create a follow-up write Task or Merge Candidate.
Test: tests/binary.rs (work_attempt_run_review_only_fails_without_followup)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only fail stops without follow-up)

WHEN any review-only reviewer artifact has verdict `uncertain` and none
has verdict `fail`,
THE SYSTEM SHALL mark the Attempt `needs-user`, write a Work handoff
artifact, and SHALL NOT create a follow-up write Task or Merge
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
branch, run configured pre-land checks, run the full merge-time reviewer
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
for a Merge Candidate with merge status `landed` and a stored
`landed_commit`,
THE SYSTEM SHALL report the stored landed commit without resolving
workspaces, rebasing, running checks, running reviewers, or moving the
target branch.
Test: tests/binary.rs (work_merge_candidate_rerun_after_cleanup_preserves_landed_state)

IF `factory work merge <work-item-id> <merge-candidate-id>` is invoked
for a Merge Candidate whose stored provenance no longer matches the
passed Attempt output,
THEN THE SYSTEM SHALL leave the target branch and stored Merge Candidate
state unchanged.
Test: tests/binary.rs (work_merge_candidate_rejects_stale_stored_provenance_without_rewrite)

WHEN the target branch has advanced since a Merge Candidate was created,
THE SYSTEM SHALL rebase the candidate workspace against the target branch
before checks, reviewers, and fast-forward merge.
Test: tests/binary.rs (work_merge_candidate_rebases_when_target_advanced)

IF the target branch moves after merge checks and reviewers run but
before the fast-forward merge,
THEN THE SYSTEM SHALL reject the merge, preserve the moved target branch,
and record merge status `failed` with a failure reason on the stored
Merge Candidate.
Test: tests/binary.rs (work_merge_candidate_rejects_target_moved_during_review)

IF rebasing the candidate workspace against the target branch fails while
`factory work merge <work-item-id> <merge-candidate-id>` executes,
THEN THE SYSTEM SHALL abort the rebase, leave the target branch
unchanged, and record merge status `failed` with a failure reason on the
stored Merge Candidate.
Test: tests/binary.rs (work_merge_candidate_rebase_failure_leaves_target_unchanged)

WHEN `factory work merge <work-item-id> <merge-candidate-id>` lands a
Merge Candidate,
THE SYSTEM SHALL record merge status `landed`, the landed commit, and
merge-time review artifacts on the stored Merge Candidate, then remove the
managed candidate worktree. If worktree cleanup fails after landing, the
system shall warn without changing the landed merge state.
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
same-invocation follow-up write budget permits another cycle,
THE SYSTEM SHALL invoke the configured Coder against the candidate
workspace with the failed merge-time review artifact paths as input
artifacts, ask the coder to address the findings and commit, verify
the workspace is clean and new commits were produced, then restart
the merge loop from rebase, checks, and merge-time review.

IF merge-time reviewer execution panics, launch-fails, or returns a
non-verdict error,
THEN THE SYSTEM SHALL leave the target branch unchanged, record merge
status `failed`, review state `failed`, the underlying error as the
failure reason, and the partial review artifacts on the stored Merge
Candidate. The merge loop SHALL NOT retry these non-verdict failures.

IF a merge-time reviewer modifies, stages, unstages, or creates files in
the candidate workspace while `factory work merge <work-item-id>
<merge-candidate-id>` executes,
THEN THE SYSTEM SHALL stop before landing, leave the target branch
unchanged, record the reviewer as non-passing, and expose an error that
names the reviewer and dirty candidate workspace. This includes ignored
files such as files under the candidate workspace's `.factory` tree.
Test: tests/binary.rs (work_merge_candidate_dirty_reviewer_fails_before_landing)
Test: tests/binary.rs (work_merge_candidate_dirty_ignored_reviewer_fails_before_landing)
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge dirty reviewer leaves target unchanged)
Test: tests/behaviors/operations/test-work-merge-candidate.sh (work merge dirty Factory state reviewer leaves target unchanged)

IF configured pre-land checks fail while `factory work merge
<work-item-id> <merge-candidate-id>` executes,
THEN THE SYSTEM SHALL leave the target branch unchanged and record merge
status `failed`, a failure reason, and check artifacts on the stored
Merge Candidate.
Test: tests/binary.rs (work_merge_candidate_failed_check_leaves_target_unchanged)

WHEN any completed review artifact has a failing verdict and the
same-invocation follow-up write budget permits another follow-up write
Task,
THE SYSTEM SHALL mark the Attempt review state as `failed` and create a
planned follow-up write Task with deterministic id
`<attempt-id>-followup-<n>`, the candidate workspace as writable access,
write Task instructions copied from explicit Work Item instructions or
derived from the Work Item planning context, and the failed review
artifacts as Task inputs.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_failed_reviews)
Test: tests/binary.rs (work_create_planning_context_feeds_followup_for_failed_reviews)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop plans follow-up write)

WHEN `factory work attempt run` advances follow-up write Tasks during one
command invocation,
THE SYSTEM SHALL count already-planned follow-up write Tasks and
follow-up write Tasks planned during the invocation against the same
two-follow-up budget.
Test: tests/binary.rs (work_attempt_run_counts_already_planned_followup_against_budget)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop counts preplanned follow-up against budget)

IF a failed review round would require a third follow-up write Task in
the same `factory work attempt run` command invocation,
THE SYSTEM SHALL mark the Attempt as `needs-user`, write a durable
handoff that names the failed review artifacts and budget exhaustion,
and SHALL NOT create the over-budget follow-up write Task.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_failed_reviews)
Test: tests/binary.rs (work_attempt_run_counts_already_planned_followup_against_budget)
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

WHEN a `Coder::run` invocation exits non-zero and its transcript
file contains a session-limit or rate-limit marker,
THE SYSTEM SHALL sleep for
`FACTORY_RATE_LIMIT_RETRY_AFTER_SECS` seconds (default 1800) and
retry the same Coder invocation, up to two retries before
propagating the original exit code as a Task failure.
Test: src/coder.rs (transcript_rate_limit_tests::detects_session_limit_marker)
Test: src/coder.rs (transcript_rate_limit_tests::detects_generic_rate_limit_phrase)
Test: src/coder.rs (transcript_rate_limit_tests::no_marker_returns_false)

WHEN no transcript file is configured for a Coder invocation,
THE SYSTEM SHALL propagate the original exit code without rate-
limit retry, since transient failure cannot be detected without
the transcript content.

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

WHEN the approach is approved by the user,
THE SYSTEM SHALL keep the approach available for Work Item planning
context and set legacy status to `approach-designed` only when using the
legacy fallback.

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
THE SYSTEM SHALL wait 5 minutes and restart the agent.
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

## Legacy Fargate execution

WHEN legacy `factory run --runtime fargate` is invoked,
THE SYSTEM SHALL upload the worktree to S3, start an ECS Fargate task,
record `runtime=fargate`, and record the ECS task handle in the source
run directory.
Test: tests/binary.rs (run_fargate_launch_uploads_workspace_and_records_task_handle), tests/behaviors/operations/test-fargate-launch.sh

WHEN legacy `factory run --runtime fargate --coder codex` is invoked,
THE SYSTEM SHALL fail with a clear unsupported-coder error.
Test: tests/binary.rs (run_fargate_with_codex_fails_before_config)

WHEN the Fargate task starts,
THE SYSTEM SHALL pull the workspace from S3 and run the Rust session loop
in the downloaded workspace while preserving `runtime=fargate` and the
ECS task handle in the run directory.
Test: tests/binary.rs (run_in_place_can_preserve_run_metadata), tests/behaviors/operations/test-fargate-entrypoint.sh

WHEN the Fargate task reaches a terminal status,
THE SYSTEM SHALL upload the workspace to S3.
Test: tests/behaviors/operations/test-fargate-entrypoint.sh

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
stale complete and landed runs by default.
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

WHEN `factory land` validates a complete run with `review-state.json`,
THE SYSTEM SHALL use that file as the effective review state before
consulting current review artifacts.
Test: src/run.rs (review-state tests), tests/binary.rs (land_accepts_review_limit_state_with_stale_fail_artifact)

WHEN `factory land` validates a complete run with review state `passed`
or `accepted-review-limit`,
THE SYSTEM SHALL treat the review state as accepted.
Test: src/run.rs (test_reviews_passed_prefers_review_state), tests/binary.rs (land_accepts_review_limit_state_with_stale_fail_artifact)

WHEN `factory land` validates a complete run with review state `failed`,
`uncertain`, or malformed JSON,
THE SYSTEM SHALL refuse to land.
Test: src/run.rs (test_reviews_passed_rejects_failed_review_state, test_reviews_passed_rejects_malformed_review_state)

WHEN `factory summary`, the generated run report, or the dashboard shows
a run with `review-state.json`,
THE SYSTEM SHALL use the recorded review state when presenting the run's
effective review outcome.
Test: src/summary.rs (summarize_prefers_review_state), src/report.rs (test_generate_report_prefers_review_state), src/dashboard.rs (test_run_view_review_state_summary_prefers_state_file)

## Parent death detection

WHILE `factory watch` is running,
IF the parent process exits (ppid changes),
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

WHEN `factory resume --help` is shown,
THE SYSTEM SHALL list `--no-sandbox` as a resume option.
Test: tests/binary.rs (resume_help_lists_no_sandbox)

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

## Land

WHEN `factory land` is invoked and the run status is not `complete`,
THE SYSTEM SHALL refuse and exit non-zero.
Test: tests/behaviors/operations/test-land.sh (land rejects non-complete run), tests/binary.rs (land_rejects_non_complete_run)

WHEN `factory land` is invoked for a run without `review-state.json` and
any current review artifact has verdict `fail`, `uncertain`, or is
missing a verdict line,
THE SYSTEM SHALL refuse and exit non-zero.
Test: tests/behaviors/operations/test-land.sh (land rejects fail review verdict, land rejects uncertain review verdict), tests/binary.rs (land_rejects_failed_reviews, land_rejects_live_failed_reviews)

WHEN `factory land [RUN_ID]` validates status and review artifacts before
landing,
THE SYSTEM SHALL prefer live worktree status and review artifacts before
falling back to source run artifacts.
Test: tests/binary.rs (land_rejects_live_failed_reviews), tests/behaviors/operations/test-live-run-state.sh (land uses live status and reviews)

WHEN the project has no executable `.factory/hooks/check-pre-land`,
THE SYSTEM SHALL run `factory land` without requiring project checks.
Test: tests/binary.rs (land_completes_full_lifecycle)

WHEN `.factory/hooks/check-pre-land` exists and is executable,
THE SYSTEM SHALL run it in the run worktree before removing the
worktree, rebasing, merging, or marking the run landed, with Factory
context (`FACTORY_HOOK`, `FACTORY_ARTIFACT_DIR`, and any available
work/attempt/task identifiers) exposed as environment variables and
stdout+stderr captured to `<run_dir>/hooks/check-pre-land.log`.
Test: tests/binary.rs (land_runs_configured_check_before_landing)

WHEN `check-pre-land` exits non-zero and no executable
`.factory/hooks/fix-pre-land` is present,
THE SYSTEM SHALL exit non-zero, keep the worktree intact, keep the run
unlanded, and print the hook's exit code and the path to its captured
log file.
Test: tests/binary.rs (land_runs_configured_check_before_landing)

WHEN `check-pre-land` exits non-zero and an executable
`.factory/hooks/fix-pre-land` is present,
THE SYSTEM SHALL require no uncommitted changes outside `.factory`
before running `fix-pre-land`, run `fix-pre-land` in the run worktree,
commit project changes outside `.factory` when the fix changes project
files, rerun reviewers after the autofix commit, rerun `check-pre-land`,
and continue landing only if `fix-pre-land` succeeds, the rerun
reviewers pass, and the recheck passes.
Test: tests/binary.rs (land_refuses_autofix_when_worktree_has_user_changes, land_autofixes_and_reruns_reviewers)

WHEN `fix-pre-land` changes files and the subsequent reviewer rerun
fails or is uncertain,
THE SYSTEM SHALL keep the worktree intact, leave the run unlanded, copy
the new review artifacts to the source run directory, and exit non-zero.
Test: tests/binary.rs (land_keeps_worktree_when_autofix_review_fails)

WHEN `factory land` is invoked and the run worktree has tracked changes,
staged changes, or untracked non-ignored files outside `.factory`,
THE SYSTEM SHALL refuse and exit non-zero.
Test: tests/binary.rs (land_rejects_dirty_completed_worktree)

WHEN `factory land` completes successfully,
THE SYSTEM SHALL copy sessions/, sessions.log, reviews/, report.md, and
status from the worktree back to the source run directory.
Test: tests/behaviors/operations/test-land.sh (land copies artifacts from worktree), tests/binary.rs (land_completes_full_lifecycle)

WHEN `factory land` completes successfully,
THE SYSTEM SHALL remove the worktree, rebase the run branch onto the
source branch, fast-forward merge into the source branch, and delete the
run branch.
Test: tests/behaviors/operations/test-land.sh (land removes worktree, land deletes run branch, land merges run commits into main), tests/binary.rs (land_completes_full_lifecycle, land_preserves_linear_history)

WHEN `factory land` completes successfully,
THE SYSTEM SHALL set the run status to `landed`.
Test: tests/binary.rs (land_completes_full_lifecycle)

WHEN `factory land` is invoked and the rebase has conflicts,
THE SYSTEM SHALL abort the rebase, exit non-zero, and leave the
repository in a clean state.
Test: tests/behaviors/operations/test-land.sh (land fails on rebase conflict), tests/binary.rs (land_fails_on_rebase_conflict)

WHEN `factory land` is invoked without a run ID,
THE SYSTEM SHALL land the most recent complete run.
Test: tests/behaviors/operations/test-land.sh, tests/binary.rs (land_resolves_most_recent_complete_run)

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

WHEN the dashboard displays a run with status `complete` or `landed`
and that run has `report.md`,
THE SYSTEM SHALL show the run report in the activity feed by default.
Test: dashboard::tests::test_completed_run_with_report_shows_report_by_default

WHEN the dashboard displays a completed run without `report.md`,
THE SYSTEM SHALL continue to show the available transcript activity.
Test: dashboard::tests::test_completed_run_without_report_shows_author_transcript

WHEN the dashboard displays a run whose status is not `complete` or
`landed`,
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

### Dashboard keyboard navigation

WHEN the user presses `q` or Ctrl+C,
THE SYSTEM SHALL exit the dashboard and restore the terminal.

WHEN the user presses Tab,
THE SYSTEM SHALL select the next agent tab within the current run,
display that tab's transcript, report, or review artifact content in the
activity feed, reset the scroll position, and re-enable auto-scroll.

WHEN the user presses Shift-Tab,
THE SYSTEM SHALL select the previous agent tab within the current run,
display that tab's transcript, report, or review artifact content in the
activity feed, reset the scroll position, and re-enable auto-scroll.

WHEN the user presses ← or →,
THE SYSTEM SHALL select the previous or next run and display that run's
activity feed. Each run preserves its own scroll position and auto-scroll
state independently.

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

WHEN the user presses PgUp or PgDn,
THE SYSTEM SHALL scroll the activity feed by 20 lines. If PgDn reaches
the bottom, auto-scroll re-enables.
Test: dashboard::tests::test_page_down_reenables_auto_scroll_at_bottom

### Dashboard mouse scroll

WHEN the user scrolls the mouse wheel up,
THE SYSTEM SHALL scroll the activity feed up by 3 lines and disable
auto-scroll.

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

### Dashboard render and poll cadence

WHILE the dashboard is running,
THE SYSTEM SHALL render frames at ~75ms intervals for smooth animation
and poll for new data at ~2s intervals to avoid unnecessary I/O.

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
THE SYSTEM SHALL run configured pre-land checks for each child run and
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
