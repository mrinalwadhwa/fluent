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

WHEN `factory work create <id> --title <title>` is invoked from a
directory,
THE SYSTEM SHALL create `.factory/work/items/<id>.json` containing a
Work Item with that id, that title, and an empty `attempts` list.
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

WHEN `factory work list` is invoked,
THE SYSTEM SHALL read stored Work Items from `.factory/work/items/` and
print each Work Item with its id and title.
Test: tests/binary.rs (work_list_outputs_stored_work_items)
Test: tests/behaviors/operations/test-work-inspection.sh (work list prints stored Work Items)

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

WHEN `factory work task run <work-item-id> <attempt-id> <task-id>` or
`factory work attempt run <work-item-id> <attempt-id>` launches a
`write` Task with stored Task instructions,
THE SYSTEM SHALL include those instructions in the coder prompt.
Test: tests/binary.rs (work_task_run_includes_task_instructions_in_coder_prompt)
Test: tests/binary.rs (work_task_run_includes_planning_context_in_coder_prompt)
Test: tests/behaviors/operations/test-work-task-run.sh (run passes Task instructions to coder prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (task run uses durable instructions and keeps extra args out of prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (attempt run uses durable instructions and keeps extra args out of prompt)

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
under `.factory/work/artifacts/<attempt-id>/<task-id>/`.
Test: tests/binary.rs (work_review_plans_review_tasks_for_completed_attempt)
Test: tests/behaviors/operations/test-work-task-run.sh (review planning adds read-only Task without changing candidate)

IF `factory work review <work-item-id> <attempt-id>` is invoked for an
Attempt without completed write output,
THEN THE SYSTEM SHALL exit non-zero and leave stored Work Item state
unchanged.
Test: tests/binary.rs (work_review_requires_completed_write_output)
Test: tests/behaviors/operations/test-work-task-run.sh (review planning requires completed write output)

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

WHEN `factory work attempt run <work-item-id> <attempt-id>` advances an
Attempt whose write output has completed and no review round is planned
for that write output,
THE SYSTEM SHALL plan review Tasks using the existing review policy and
run planned review Tasks through the existing Task executor.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

WHEN `factory work attempt run <work-item-id> <attempt-id>` is invoked
for an Attempt with planned review Tasks,
THE SYSTEM SHALL run the planned review Tasks through the existing Task
executor before planning later transitions.
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop runs planned review Tasks)

WHEN all review Tasks for an Attempt review round complete and all
review artifacts have passing verdicts,
THE SYSTEM SHALL mark the Attempt review state as `passed`, leave the
Attempt `complete`, create one durable Merge Candidate, and report the
Merge Candidate id.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

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
branch, run configured pre-land checks, run merge-time reviewers, and
fast-forward the target branch only after those steps pass.
Test: tests/binary.rs (work_merge_candidate_lands_after_merge_time_reviews)

WHEN `factory work merge <work-item-id> <merge-candidate-id>` is invoked
for a Merge Candidate with merge status `landed` and a stored
`landed_commit`,
THE SYSTEM SHALL report the stored landed commit without resolving
workspaces, rebasing, running checks, running reviewers, or moving the
target branch.
Test: tests/binary.rs (work_merge_candidate_lands_after_merge_time_reviews)

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
<merge-candidate-id>` executes,
THEN THE SYSTEM SHALL leave the target branch unchanged and record merge
status `failed`, review state `failed`, a failure reason, and review
artifacts on the stored Merge Candidate.
Test: tests/binary.rs (work_merge_candidate_failed_review_leaves_target_unchanged)

IF configured pre-land checks fail while `factory work merge
<work-item-id> <merge-candidate-id>` executes,
THEN THE SYSTEM SHALL leave the target branch unchanged and record merge
status `failed`, a failure reason, and check artifacts on the stored
Merge Candidate.
Test: tests/binary.rs (work_merge_candidate_failed_check_leaves_target_unchanged)

WHEN any completed review artifact has a failing verdict,
THE SYSTEM SHALL mark the Attempt review state as `failed` and create a
planned follow-up write Task with deterministic id
`<attempt-id>-followup-<n>`, the candidate workspace as writable access,
the Work Item instructions copied into the Task instructions, and the
failed review artifacts as Task inputs.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_failed_reviews)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop plans follow-up write)

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

## Brief capture

WHEN the user invokes the capture-brief skill,
THE SYSTEM SHALL interview the user, research the codebase, and write
a brief for a Work Item, using `.factory/runs/[run-id]/brief.md` only
as a legacy fallback or bridge planning artifact.
Test: tests/behaviors/skills/code-reviewer.md (test-skill)

WHEN the user invokes the build-in-the-factory skill for new delegated
build work,
THE SYSTEM SHALL teach Work Items, Attempts, Tasks, Workspaces, and Merge
Candidates as the target lifecycle and describe legacy `factory run` as a
transitional fallback.
Test: tests/behaviors/operations/test-build-in-factory-work-model-guidance.sh

WHEN the brief is confirmed by the user,
THE SYSTEM SHALL leave the Work Item available for later planning and set
legacy status to `briefed` only when using the legacy fallback.

## Behavior definition

WHEN the user invokes the define-behaviors skill,
THE SYSTEM SHALL read the brief and existing behaviors, elaborate into
EARS-format behavioral statements, and write behaviors.diff.md.
Test: tests/behaviors/skills/run-summary-behaviors.md (test-skill)

WHEN behaviors are approved by the user,
THE SYSTEM SHALL set status to `behaviors-defined`.
Test: tests/behaviors/skills/run-summary-behaviors.md (test-skill)

## Approach design

WHEN the user invokes the design-approach skill,
THE SYSTEM SHALL research external systems, evaluate options, and write
approach.md with relevant expertise references, key technical decisions,
and solution direction.

WHEN the approach is approved by the user,
THE SYSTEM SHALL set status to `approach-designed`.

## Execution planning

WHEN the user invokes the plan-execution skill,
THE SYSTEM SHALL break the approach into executable steps and write
plan.md.

WHEN the plan is approved by the user,
THE SYSTEM SHALL set status to `planned`.

## Worktree isolation

WHEN `factory run` is invoked,
THE SYSTEM SHALL create a git worktree branched from the current HEAD,
copy the run's state into it, and execute within the worktree.
Test: src/worktree.rs (setup_run_worktree tests), tests/binary.rs (worktree creates and copies state)

WHEN `factory run` is invoked from a non-main branch,
THE SYSTEM SHALL branch the worktree from that branch and record it as
the source-branch.
Test: tests/test-run (setup_run_worktree from non-main branch)

## Session loop (local)

WHEN `factory run` is invoked with the local runtime,
THE SYSTEM SHALL launch the selected coder in non-interactive mode with
the brief or handoff as the initial prompt.
Test: src/session.rs (test_loop_initial_prompt_uses_brief, test_loop_initial_prompt_uses_handoff), tests/binary.rs (run_uses_handoff_prompt_when_handoff_exists)

WHEN `factory run --coder codex` is invoked with the local runtime,
THE SYSTEM SHALL launch Codex with `codex exec --json`, prepend the
factory system prompt to the run prompt, and capture Codex JSON output
as the session transcript.
Test: tests/binary.rs (run_with_codex_uses_exec_json_and_status_contract)

WHEN `factory run` is invoked with an unknown coder,
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

## Session observability

WHEN a session completes within the session loop,
THE SYSTEM SHALL write a line to `sessions.log` containing the session
number, exit code, duration, and status.
Test: src/session.rs (test_loop_writes_sessions_log, test_loop_writes_nonzero_exit_to_sessions_log), tests/binary.rs (run_writes_sessions_log), tests/behaviors/operations/test-observability.sh

WHEN the session loop launches an agent session,
THE SYSTEM SHALL request machine-readable JSON events from the selected
coder and pipe stdout to `sessions/session-N/transcript.jsonl`.
Test: src/session.rs (test_loop_creates_session_transcript_dir), tests/binary.rs (run_captures_stream_json_transcript), tests/behaviors/operations/test-observability.sh

## Review archiving

WHEN a review round fails and a new round starts,
THE SYSTEM SHALL archive previous review artifacts to `reviews/round-N/`
before running new reviews. Review files and transcript files are moved,
leaving top-level `reviews/` artifacts for the current round only.
Test: src/review.rs (test_archive_previous_round_moves_reviews, test_archive_previous_round_noop_for_first_round), tests/binary.rs (run_archives_review_rounds), tests/behaviors/operations/test-observability.sh

WHEN a reviewer runs,
THE SYSTEM SHALL capture its stream-json output to
`reviews/transcript-{name}.jsonl`.
Test: tests/binary.rs (run_archives_review_rounds), tests/behaviors/operations/test-observability.sh

## Session loop (local) — credential refresh

WHEN a new Claude session starts on the sandboxed local runtime,
THE SYSTEM SHALL run an unsandboxed Claude invocation to refresh the
OAuth token, then re-read the token from Keychain into the process
environment.
Test: src/session.rs (test_loop_calls_pre_session_before_each_session, test_loop_stops_when_pre_session_returns_error), tests/behaviors/operations/test-claude-runtime-hooks.sh (sandboxed claude runs refresh hook)

WHEN a new Codex session starts on the sandboxed local runtime,
THE SYSTEM SHALL NOT run the Claude credential refresh hook.
Test: tests/behaviors/operations/test-codex-runtime.sh (codex does not run claude refresh hook, parallel codex does not run claude refresh hook)

## Fargate execution

WHEN `factory run --runtime fargate` is invoked,
THE SYSTEM SHALL upload the worktree to S3, start an ECS Fargate task,
record `runtime=fargate`, and record the ECS task handle in the source
run directory.
Test: tests/binary.rs (run_fargate_launch_uploads_workspace_and_records_task_handle), tests/behaviors/operations/test-fargate-launch.sh

WHEN `factory run --runtime fargate --coder codex` is invoked,
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
THE SYSTEM SHALL display all runs with their status, runtime, and brief
summary.
Test: tests/binary.rs (status display tests, status_prefers_live_worktree_status), tests/behaviors/operations/test-live-run-state.sh (status lists live status)

WHEN `factory status` is invoked and stored Work Items exist,
THE SYSTEM SHALL display a Work Items section with each Work Item's
latest Attempt, selected Task, review state, Merge Candidate, merge
state, actionable label, and title.
Test: tests/binary.rs (status_shows_work_items_without_runs, status_shows_runs_and_work_items_together)

WHEN `factory status` is invoked for a project with Work Items and no
legacy runs,
THE SYSTEM SHALL display the Work Items section instead of reporting
that no runs were found.
Test: tests/binary.rs (status_shows_work_items_without_runs)

WHEN `factory status` reads one or more invalid Work Item files,
THE SYSTEM SHALL report the invalid Work model path in a Work Item read
errors section while still displaying valid runs and valid Work Items.
Test: tests/binary.rs (status_reports_invalid_work_item_with_valid_state), tests/behaviors/operations/test-work-status-dashboard.sh (status reports invalid Work without hiding valid state)

WHEN `factory status` is invoked after cleanup,
THE SYSTEM SHALL list cleaned runs with their existing run status and
without a cleanup-specific status.
Test: tests/behaviors/operations/test-cleanup.sh (status lists cleaned runs with original status)

WHEN `factory status` is invoked and a Fargate run exists,
THE SYSTEM SHALL display the locally recorded run status, runtime, and
brief summary without querying AWS.
Test: tests/behaviors/operations/test-status-edges.sh (status fargate uses local state without AWS)

## Run summary

WHEN `factory summary` is invoked,
THE SYSTEM SHALL summarize the active run using existing run artifacts
and print the summary to stdout.
Test: tests/binary.rs (summary_resolves_active_run)

WHEN `factory summary --run-id <id>` is invoked,
THE SYSTEM SHALL summarize that run instead of resolving the active run.
Test: tests/binary.rs (summary_uses_explicit_run_id)

WHEN `factory summary --run-id <id>` is invoked for a run with a live
worktree run directory,
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

WHEN no run can be resolved for `factory summary`,
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
Test: tests/binary.rs (cleanup_work_items_dry_run_and_apply_manage_state_worktree_and_branch), tests/binary.rs (cleanup_work_items_removes_terminal_merge_candidate_artifacts_and_worktree)

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

WHEN the dashboard opens without an explicit run,
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

## Review runs

WHEN `factory run` is invoked and the run's mode is `review`,
THE SYSTEM SHALL set status to `reviewing`, run reviewers with
full-codebase scope, and produce findings. No author session is launched.
Test: src/session.rs (review-only mode tests)

WHEN `factory run` is invoked and the run has a `scope` file,
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

WHEN the project has no `.factory/config.toml`,
THE SYSTEM SHALL run `factory land` without requiring project checks.
Test: tests/binary.rs (land_completes_full_lifecycle)

WHEN `.factory/config.toml` defines a check with `run_before_land = true`,
THE SYSTEM SHALL run the check command in the run worktree before
removing the worktree, rebasing, merging, or marking the run landed.
Test: tests/binary.rs (land_runs_configured_check_before_landing)

WHEN a pre-land check fails and has no enabled autofix command,
THE SYSTEM SHALL exit non-zero, keep the worktree intact, keep the run
unlanded, and print the check name, failed command, command output, and
configured fix command if present.
Test: tests/binary.rs (land_runs_configured_check_before_landing)

WHEN a pre-land check fails and has `autofix = true` with a
`fix_command`,
THE SYSTEM SHALL require no uncommitted changes outside `.factory`
before running the fix command, run the fix command in the run worktree,
commit project changes outside `.factory` when the fix changes project
files, rerun pre-land checks, rerun reviewers after an autofix commit,
and continue landing only if the required checks and reviews pass.
Test: tests/binary.rs (land_refuses_autofix_when_worktree_has_user_changes, land_autofixes_and_reruns_reviewers)

WHEN an autofix command changes files and the subsequent reviewer rerun
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
THE SYSTEM SHALL display a TUI listing all runs with their status,
an activity feed for the selected transcript or report view, and
keyboard navigation.
Test: tests/behaviors/operations/test-dashboard.sh

WHEN `factory dashboard` is invoked and stored Work Items exist,
THE SYSTEM SHALL provide a Work Items view that shows Work Items,
latest Attempts, selected Tasks, review state, Merge Candidates, merge
state, and actionable labels.
Test: dashboard::tests::test_work_view_renders_work_items_without_runs,
tests/behaviors/operations/test-work-status-dashboard.sh (dashboard shows
Work Items alongside legacy runs)

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

WHEN the dashboard is displayed,
THE SYSTEM SHALL show a phase label that accurately describes what is
happening right now (executing, reviewing, complete, failed, needs input,
rate-limited, planned). The `reviewing` phase shows a spinner, including
before reviewer transcripts exist.
Test: dashboard::tests::test_header_reviewing_shows_progress, dashboard::tests::test_header_complete_no_spinner, dashboard::tests::test_header_failed_no_spinner, dashboard::tests::test_compute_phase_needs_user, dashboard::tests::test_compute_phase_rate_limited, dashboard::tests::test_header_rate_limited_shows_spinner, dashboard::tests::test_compute_phase_planned, tests/behaviors/operations/test-dashboard-activity.sh (no crash when run has failed, no crash when run needs user input, no crash with mixed run states), tests/behaviors/operations/test-dashboard-review-rounds.sh (reviewing status shows active work before transcripts)

### Dashboard layout

WHEN the dashboard is displayed,
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

## Sandbox (local)

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

WHEN `factory run --coder codex` is invoked with the sandboxed local runtime,
THE SYSTEM SHALL launch Codex under `sandbox-exec` with Factory's
Seatbelt profile, approval policy `never`, and the run worktree as
`--cd`, while disabling Codex's own sandbox. The rendered profile SHALL
include `common.sb` plus the Codex-specific `codex.sb` layer. The
Codex process SHALL receive `SSL_CERT_FILE` for a file-based CA bundle
selected by Factory, even when the caller inherited a different
`SSL_CERT_FILE`.
Test: tests/behaviors/operations/test-codex-runtime.sh (sandboxed codex uses factory seatbelt, sandboxed codex prefers factory ca bundle), tests/behaviors/operations/test-codex-approval-flag.sh (approval-policy flag appears before exec)

WHEN `factory run --coder codex --no-sandbox` is invoked,
THE SYSTEM SHALL launch Codex with
`--dangerously-bypass-approvals-and-sandbox`.
Test: tests/binary.rs (run_with_codex_uses_exec_json_and_status_contract)

WHILE running inside the sandbox,
THE SYSTEM SHALL inject credentials via environment variables, never by
opening filesystem access to credential stores.
Test: tests/behaviors/operations/test-sandbox.sh (profile denies Keychain Mach services, profile denies credential filesystem access, credentials injected via env vars)
