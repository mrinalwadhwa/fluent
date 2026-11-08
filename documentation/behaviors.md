# Behaviors

Observable behaviors of the fluent system. Each statement describes what
the system does, not how. EARS format.

## Native Codex startup

### B1

WHEN a native Codex Task starts its first authenticated turn on macOS,
THE SYSTEM SHALL complete that turn successfully inside the production worker
sandbox, retaining the interactive-home and Keychain denials, and record the
successful completion for the same execution generation, assigned conversation,
and first turn that started.
Test: tests/native_agent_tasks.rs (host_first_turn_probe_requires_successful_completion)
Test: tests/native_agent_tasks.rs (host_first_turn_probe_rejects_failed_or_uncorrelated_completion)
Test: {"kind":"command","command":"bash tests/host/check-native-codex-first-turn.sh","command_index":2,"description":"authenticated first-turn completion under the production macOS worker sandbox"}

## Dashboard operator console

### B1

WHEN the internal compatibility snapshot opens, THE SYSTEM SHALL show grouped
Current Work and select the first Work Item that needs attention, is running, or
is ready. The project dashboard follows the project layout and navigation below.
Test: src/dashboard/tests.rs (dashboard_opens_on_current_work)

### B2

WHEN an operator navigates, filters, refreshes, or resizes the dashboard, THE
SYSTEM SHALL preserve its selected Work Item or worker when it remains visible
in the counted view and select the nearest remaining row when it does not.
Test: src/dashboard/tests.rs (selection_survives_refresh_reorder_and_filter)

### B3

WHEN the dashboard refresh fails after a successful load, THE SYSTEM SHALL keep
the displayed snapshot and label it stale until a later successful refresh.
Test: src/dashboard/tests.rs (failed_poll_keeps_snapshot_and_marks_it_stale)

### B4

WHEN the dashboard receives navigation, filtering, selection, detail or terminal
inspection, help, copy, or refresh input, THE SYSTEM SHALL not modify stored Work
state.
Test: src/dashboard/tests.rs (dashboard_controls_leave_work_state_unchanged)

## Project dashboard layout and navigation

### B1

WHEN the project dashboard renders at 100 columns or wider, THE SYSTEM SHALL
show two compact header lines with counted All, Needs you, Running, Ready,
Waiting, and History views, one horizontal rule, and one continuous divider
between the project list and details or terminal, without pane boxes.
Test: src/dashboard/tests.rs (project_layout_uses_header_rule_and_one_pane_divider)

### B2

WHEN project work renders, THE SYSTEM SHALL use compact aligned rows with exact
categories, recorded update times when available, a highlighted selection marker,
and stale identification beside the source group. Ready SHALL include Ready and
Ready to land; Waiting SHALL include Queued and Planned.
Test: src/dashboard/tests.rs (project_rows_preserve_exact_status_and_selection_cues)
Test: src/dashboard/tests.rs (project_rows_show_recorded_update_times)

### B3

WHEN a Work Item or worker is selected, THE SYSTEM SHALL prioritize identity,
reason or state, and canonical next action without repeating the selected title
and category; worker Task, invocation, and terminal availability SHALL remain
available in details.
Test: src/dashboard/tests.rs (project_details_prioritize_action_without_redundant_row_copy)

### B4

WHEN navigation owns input and Tab or Shift+Tab is pressed, THE SYSTEM SHALL
cycle counted views forward or backward and preserve a selection that remains
visible.
Test: src/dashboard/tests.rs (tab_views_preserve_selection_and_exact_categories)

### B5

WHEN Enter is pressed on a Work Item, THE SYSTEM SHALL expand or collapse its
workers. WHEN Right is pressed on a worker, THE SYSTEM SHALL open only that
worker's available session in Observe or show why it is unavailable, without
starting or retargeting Work.
Test: src/dashboard/tests.rs (arrow_drill_requires_selected_available_worker)
Test: src/dashboard/tests.rs (arrow_drill_observes_without_retargeting_on_selection)

### B6

WHEN the project dashboard has room for one pane, THE SYSTEM SHALL show the list
first, use Right to inspect details, and use Left/Esc to return while retaining
Tab/Shift+Tab for views when navigation owns input.
Test: src/dashboard/tests.rs (narrow_view_keeps_filter_and_return_keys_distinct)

### B7

WHEN an agent terminal is displayed, THE SYSTEM SHALL keep its Observe, Control,
or navigation-with-Control state and the relevant return command visible.
Test: src/dashboard/tests.rs (terminal_mode_and_focus_remain_explicit)

### B8

WHEN a terminal pane becomes hidden or too small, THE SYSTEM SHALL release input
focus and retain the existing Control lease rules; enlarging the pane SHALL not
restore focus automatically.
Test: src/dashboard/tests.rs (hidden_terminal_releases_input_focus)
Test: src/dashboard/tests.rs (hidden_terminal_rejects_native_input)

## Scheduled public verification

### B1

WHEN a candidate requires an approved public verification command,
THE SYSTEM SHALL persist one schedulable verification Task bound to the exact
Work Item, Attempt, candidate commit, command identity, working directory, and
checker definition before reporting that verification as executing.
Test: tests/binary.rs (focused_evidence_run_schedules_candidate_bound_verification_task)

### B2

WHEN the scheduler runs a verification Task,
THE SYSTEM SHALL execute the command in a worker with read-only candidate access,
a private writable scratch area, bounded logs, the existing credential-denial
policy, and the ordinary Task lease and process-settlement lifecycle.
Test: tests/binary.rs (scheduled_verification_task_runs_in_confined_worker)

### B3

WHEN a verification worker finishes,
THE SYSTEM SHALL retain detailed stdout and stderr in the Task artifact area and
publish a compact immutable receipt that binds the candidate commit, command,
working directory, checker digest, result, timestamps, exit status, and artifact
digests; only a valid receipt may advance evidence review.
Test: tests/binary.rs (verification_receipt_is_compact_immutable_and_candidate_bound)

### B4

IF a verification fixture, receipt, candidate identity, checker digest, or
artifact digest is missing, malformed, stale, or inconsistent,
THEN THE SYSTEM SHALL reject it before readiness without changing the candidate
or claiming the check passed; a corrected worker result for the same authorized
frontier may subsequently be accepted.
Test: tests/binary.rs (invalid_verification_receipt_blocks_then_corrected_receipt_advances)

### B5

WHILE a verification worker is running for one Attempt,
THE SYSTEM SHALL release project-global coordination locks so an unrelated
Attempt can be scheduled and make durable progress.
Test: tests/binary.rs (verification_worker_does_not_block_unrelated_attempt_progress)

## Terminal cancellation ownership

The terminal runtime is available for future native worker integration;
ordinary workers currently remain headless. Managed terminals bind attachment
to an execution generation and retain their
worker when a client detaches. Observe is read-only; a single process-held
Control lease permits input and releases on client death. A changed or ended
generation rejects attachment. Existing headless workers, completed Tasks,
and deterministic Tester workers explain why they have no live agent terminal.
Cancellation must settle the server and pane descendants before success.

### B1

WHEN an operator cancels a local Attempt,
THE SYSTEM SHALL first persist one durable cancellation intent that prevents any
new Task, review phase, Tester, verification, or Learner launch for that Attempt,
then stop and settle every already-owned local execution.
Test: tests/binary.rs (attempt_cancel_persists_stop_before_settling_active_execution)
Test: tests/binary.rs (attempt_cancel_during_scheduler_startup_blocks_delayed_launch)
Test: src/work_task_executor.rs (attempt_stop_blocks_reserved_peer_while_another_worker_settles)

### B2

WHEN cancellation races with setup or launch,
THE SYSTEM SHALL expose a cancellable execution identity before any Task appears
executing, make the launcher recheck the durable stop boundary, and retain that
boundary until the child enters its requested program. The child SHALL publish
a generation-bound PID and process-group receipt before that entry, so recovery
can validate and stop it even if the parent dies before publishing `running`.
Test: src/work_task_executor.rs (task_execution_identity_precedes_executing_state_and_honors_stop)
Test: tests/execution_handoff.rs (launch_handoff_publishes_identity_before_payload, launch_handoff_recovers_child_after_owner_crash, launch_handoff_cancel_serializes_with_payload_start)
Test: src/execution.rs (cancel_settled_execution_blocks_later_children_until_reservation)
Test: src/work_attempt_loop.rs (canceled_attempt_startup_requires_resume_authorization, cancellation_resume_authority_rejects_a_later_stop)
Test: src/queue.rs (resume_records_exact_cancellation_authority)
Test: src/execution.rs (attempt_owner_survives_commands_and_heartbeat, owned_launch_rejects_a_missing_attempt)

### B3

WHEN cancellation occurs during Writer, scheduled verification, Tester, review,
or Learner execution,
THE SYSTEM SHALL stop that owned worker, settle its process tree, suspend the
same Attempt for explicit resume, and launch no downstream stage.
A pending reviewed Merge Candidate SHALL remain storable across that durable
stop but SHALL NOT advance while cancellation is active. Its Attempt SHALL stay
`Complete` with no `Interrupted` pause kind; the typed cancellation record SHALL
block new execution and readiness advancement. Explicit resume SHALL
preserve its reviewed identity and prior readiness timestamp and SHALL require
succeeded Learning before advancement.
Test: src/work_model.rs (canceled_pending_candidate_stays_valid_but_cannot_advance)
Test: src/work_attempt_loop.rs (canceled_pending_candidate_resumes_reviewed_state)
Test: tests/binary.rs (attempt_cancel_stops_each_stage_without_downstream_launch)
Test: tests/binary.rs (attempt_cancel_settles_learner_waiting_for_provider_capacity)
Test: tests/binary.rs (attempt_cancel_between_tester_commands_blocks_next_child, attempt_cancel_between_tester_command_and_extractor_blocks_next_child)

### B4

WHEN the same cancellation is requested again after the Attempt is suspended,
THE SYSTEM SHALL return the already-settled result without requiring another
active process and without mutating history a second time.
Test: tests/binary.rs (attempt_cancel_is_idempotent_after_settlement)

## Dashboard verification journeys

### B1

WHEN a user opens dashboard help in a populated terminal,
THE SYSTEM SHALL show usable navigation guidance and close help through its
documented control; verification SHALL assert these state transitions and
controls rather than require one incidental prose word.
Test: tests/behaviors/operations/test-work-status-dashboard.sh (test_dashboard_help_journey)

### B2

WHEN Work-item read errors overflow the visible dashboard region and the user
presses `e`,
THE SYSTEM SHALL expose the error view or error content, allow return to normal
navigation, and leave every stored Work file byte-for-byte unchanged.
Test: tests/behaviors/operations/test-work-status-dashboard.sh (test_dashboard_overflow_error_journey)

## Read-only Work snapshots

### B1

WHEN `fluent status` or `fluent dashboard` loads stored Work,
THE SYSTEM SHALL leave all files and directories under `.fluent/work`
byte-for-byte and entry-for-entry unchanged.
Test: tests/work_model_external.rs (status_snapshot_is_nonmutating)
Test: src/dashboard/tests.rs (dashboard_snapshot_is_nonmutating)

### B2

WHEN a pending Work transaction exists during a status or dashboard load,
THE SYSTEM SHALL display the last published Work Item when it can be read
consistently and report pending host-owned recovery without applying or deleting
the journal.
Test: tests/work_model_external.rs (snapshot_reports_pending_transaction_without_recovery)
Test: src/work_model.rs (snapshot_reads_unchanged_children_with_pending_transaction)
Test: src/work_model.rs (snapshot_rejects_interrupted_child_publication)

### B3

WHEN a legacy artifact path is present during a status or dashboard load,
THE SYSTEM SHALL resolve status from the stored path without moving the artifact
or rewriting the record.
Test: tests/work_model_external.rs (snapshot_does_not_migrate_legacy_artifacts)

### B4

IF one Work Item, split record, or pending transaction cannot be read safely,
THEN THE SYSTEM SHALL keep other valid Work Items visible and include a bounded
diagnostic for the unreadable item.
Test: tests/work_model_external.rs (snapshot_isolates_item_and_transaction_errors)
Test: src/work_model.rs (snapshot_retries_publication_between_root_and_children)
Test: src/work_model.rs (snapshot_defers_legacy_journal_without_base_proof)

## Read-only execution liveness

### B1

WHEN status or dashboard checks a running Task whose execution identity lock is absent,
THE SYSTEM SHALL report the execution as not live without creating the lock or
its parent directory.
Test: src/execution.rs (liveness_probe_does_not_create_identity_lock)

### B2

WHEN an existing execution identity lock is held by the recorded process tree,
THE SYSTEM SHALL report the execution as live using a read-only open of that lock.
Test: src/execution.rs (liveness_probe_observes_existing_held_identity_lock)

## Explicit maintenance ownership

### B1

WHEN an authorized lifecycle mutation begins for a Work Item with a valid pending transaction,
THE SYSTEM SHALL recover that transaction under the Work Item model lock before
applying the new mutation.
Test: src/work_model.rs (mutating_boundary_recovers_pending_transaction)

### B2

WHEN the operator runs `fluent work-item upgrade` for a legacy artifact path,
THE SYSTEM SHALL perform and audit the required format and artifact migration.
Test: src/work_model.rs (explicit_upgrade_owns_legacy_artifact_migration)

## Published-reader compatibility fence

### B1

WHEN current Fluent writes a Work Item split record,
THE SYSTEM SHALL encode a validated writer envelope that the published v0.3.0
Work Item deserializer cannot accept.
Test: tests/work_model_external.rs (current_record_has_old_reader_wire_fence)

### B2

WHEN current Fluent reads a legacy v0.3.0 Work Item with the original writer-version string,
THE SYSTEM SHALL preserve that Work Item without rewriting it during read-only observation.
Test: tests/work_model_external.rs (current_reader_accepts_legacy_writer_string_without_rewrite)

### B3

WHEN the published v0.3.0 binary attempts a mutating command on a Work Item written by current Fluent,
THE SYSTEM SHALL make the older command fail before changing any Work Item,
Attempt, Task, Merge Candidate, journal, or artifact.
Test: tests/behaviors/operations/test-work-model-old-reader-fence.sh

### B4

WHEN current Fluent mutates its own fenced record,
THE SYSTEM SHALL preserve current fields and normal optimistic revision and
transaction behavior.
Test: tests/work_model_external.rs (current_reader_round_trips_fenced_record)

## Local execution recovery

### B1

WHEN Fluent launches a local coder Task, THE SYSTEM SHALL persist its owner,
process-group identity, lifecycle state, and heartbeat beside the Task transcript.
Test: src/coder.rs (successful_capture_returns_coder_exit_and_persists_bytes)
Test: tests/execution_handoff.rs (launch_handoff_publishes_identity_before_payload)

### B2

WHEN an operator cancels a locally executing Attempt, THE SYSTEM SHALL signal
only the process group whose inherited identity lock matches the persisted
record, confirm it has stopped, and pause the same Attempt as `interrupted`.
Test: src/work_attempt_loop.rs (cancel_local_attempt_stops_owned_group_and_replans_once)
Test: src/execution.rs (stale_record_never_signals_reused_identity)
Test: tests/execution_handoff.rs (launch_handoff_rejects_unbound_identity, launch_handoff_preserves_owned_descendants, launch_handoff_settled_record_preserves_owned_descendant)

### B3

WHEN an operator resumes an `interrupted` Attempt, THE SYSTEM SHALL replan each
canceled Task once while preserving completed Tasks and prior artifacts.
Test: src/work_attempt_loop.rs (cancel_local_attempt_stops_owned_group_and_replans_once)

### B4

WHEN macOS reports `EPERM` while Fluent sweeps the process group of an observed
exited coder leader, THE SYSTEM SHALL classify the group as already gone while
still delivering `SIGKILL` when a live same-user descendant remains.
Test: src/coder.rs (exited_macos_group_eperm_means_no_signalable_members)
Test: src/coder.rs (natural_exit_sweeps_group_before_reaping)

### B5

WHEN a current Fluent reader sees a supported legacy execution record, THE
SYSTEM SHALL retain its established ownership and stop rules and report a live
legacy identity without guessing its process group.
Test: tests/execution_handoff.rs (launch_handoff_legacy_records_fail_closed)

### B6

WHEN an older supported Fluent binary sees a version 2 launch record, THE
SYSTEM SHALL reject that record before mutating execution or Attempt state.
Test: tests/execution_handoff.rs (launch_handoff_old_reader_rejects_new_protocol)

## Enclosing agent sandbox handoff

### B1

WHEN configured `fluent init` runs inside a host that cannot apply Fluent's
macOS worker sandbox, THE SYSTEM SHALL stop before provider authentication or
project mutation and report `FLUENT_HOST_SANDBOX_BLOCKED`.
Test: tests/binary.rs (configured_init_checks_host_sandbox_before_provider_or_mutation)

### B2

WHEN a sandboxed local `fluent attempt run` cannot apply Fluent's worker
sandbox, THE SYSTEM SHALL stop before provider authentication, Task reservation,
worktree creation, or Attempt mutation.
Test: tests/binary.rs (attempt_run_checks_host_sandbox_before_provider_or_state_change)

### B3

WHEN a newly elected local scheduler cannot apply Fluent's worker sandbox, THE
SYSTEM SHALL stop before it prepares a baseline, polls, claims queued Work, or
creates an Attempt.
Test: tests/binary.rs (scheduler_checks_host_sandbox_before_claiming_queued_work)

### B4

IF the enclosing Codex or Claude Code sandbox blocks Fluent's worker sandbox,
THEN THE SYSTEM SHALL direct the operator or host agent to rerun the same Fluent
command through per-command approval outside the enclosing sandbox.
Test: src/os.rs (host_sandbox_failure_explains_scoped_agent_recovery)

### B5

IF Fluent presents recovery for an enclosing agent sandbox, THEN THE SYSTEM
SHALL state that Fluent's worker sandbox remains active for the retried command.
Test: src/os.rs (host_sandbox_failure_explains_scoped_agent_recovery)

### B6

IF Fluent presents recovery for an enclosing agent sandbox, THEN THE SYSTEM
SHALL NOT recommend Fluent's `--no-sandbox` option or full-session host-agent
access.
Test: src/os.rs (host_sandbox_failure_explains_scoped_agent_recovery)

## Autonomous Claude confinement

### B1

WHEN Fluent launches an autonomous Claude Writer, THE SYSTEM SHALL set `HOME` to
an Attempt-scoped managed artifact directory and deny reads and writes to the
operator's persistent Claude project and memory tree.
Test: src/work_task_executor.rs (claude_writer_launch_uses_managed_home)
Test: src/os.rs (autonomous_claude_profile_denies_operator_project_state)
Test: src/os.rs (autonomous_claude_profile_confines_memory_writes_to_worker_home)

## Audited round-cap continuation

### B1

WHEN an operator approves one to three additional Writer rounds for a legitimate
`round-cap` pause, THE SYSTEM SHALL append an approval bound to the candidate
commit and SHA-256 digests of the current failed correction artifacts without
planning or running a Writer.
Test: tests/binary.rs (attempt_extend_binds_candidate_and_failed_review_bytes)
Test: src/work_model.rs (round_cap_extension_is_bounded_idempotent_and_digest_bound)
Test: src/work_attempt_loop.rs (final_tester_gate_round_cap_frontier_binds_all_correction_bytes)

### B2

IF the candidate, failed correction set, or failed correction bytes change after a
round-cap approval, THEN THE SYSTEM SHALL reject resume without changing the
Attempt frontier.
Test: tests/binary.rs (attempt_extend_binds_candidate_and_failed_review_bytes)
Test: src/work_attempt_loop.rs (final_tester_gate_round_cap_frontier_binds_all_correction_bytes)

### B3

WHEN a failed final Tester reaches the Writer-round cap before a candidate gate
is created, THE SYSTEM SHALL accept a bounded continuation from its structured
result and exact failed-command logs only when it is the latest completed final
Tester after the latest Writer and names that Writer's exact candidate commit.
Test: tests/binary.rs (attempt_extend_accepts_candidate_bound_failed_final_tester_without_gate)
Test: src/work_attempt_loop.rs (structured_final_tester_failure_without_gate_is_an_approvable_round_cap_frontier)

### B4

IF a candidate gate exists or no latest candidate-bound final Tester failure can
be verified, THEN THE SYSTEM SHALL NOT substitute ungated Tester evidence for
the round-cap correction frontier.
Test: src/work_attempt_loop.rs (passing_final_tester_without_gate_is_not_a_round_cap_frontier)
Test: src/work_attempt_loop.rs (failed_tester_before_latest_writer_is_not_a_round_cap_frontier)
Test: src/work_attempt_loop.rs (failed_command_recheck_is_not_an_ungated_final_tester_frontier)
Test: src/work_attempt_loop.rs (failed_final_tester_without_gate_rejects_candidate_mismatch)
Test: src/work_attempt_loop.rs (failed_final_tester_without_gate_rejects_missing_exact_log)
Test: src/work_attempt_loop.rs (failed_final_tester_without_gate_rejects_malformed_results)
Test: src/work_attempt_loop.rs (missing_final_tester_results_without_gate_is_not_a_round_cap_frontier)
Test: src/work_attempt_loop.rs (final_tester_gate_round_cap_frontier_binds_all_correction_bytes)

### B5

WHEN one or more evidence-targeted `code-change` reviews produce scoped Writers
and the Attempt later pauses at `round-cap`, THE SYSTEM SHALL accept a bounded
extension from the actual Writer frontier.
Test: src/work_model.rs (evidence_code_changes_advance_the_effective_write_cap)
Test: tests/binary.rs (attempt_extend_counts_repeated_evidence_corrections)

### B6

WHEN an operator authorizes replacement `Test:` references for selected behavior
requirements on a failed-review `round-cap` Attempt, THE SYSTEM SHALL record the
old and new references and authorization and update the Work Item planning
context and Attempt completion contract to the same selected references.
Test: src/planning_reference_recovery.rs (public_recovery_revises_selected_test_references)

### B7

WHEN Fluent accepts a Test-reference revision, THE SYSTEM SHALL preserve the
candidate, prior review history, adopted ownership, and accepted host evidence,
and schedule fresh Behaviors and Tests reviews without consuming a Writer round.
Test: src/planning_reference_recovery.rs (public_recovery_revises_selected_test_references)
Test: tests/binary.rs (legacy_ownership_survives_test_reference_recovery)
Test: tests/binary.rs (later_writer_host_proof_survives_test_reference_recovery)
Test: tests/binary.rs (explicit_plan_host_proof_survives_test_reference_recovery)
Test: tests/binary.rs (interleaved_planning_revisions_retain_host_proof)

### B8

IF the selected requirement is not a behavior, the Attempt is no longer at the
authorized frontier, or planning bytes changed before the revision commits, THEN
THE SYSTEM SHALL reject the Test-reference revision without changing the Attempt.
Test: src/planning_reference_recovery.rs (rejects_unknown_behavior_without_partial_output)

### B9

WHEN the same accepted Test-reference revision is repeated, THE SYSTEM SHALL
reuse its audit and schedule no additional reviews.
Test: src/planning_reference_recovery.rs (public_recovery_revises_selected_test_references)

## Historical round-cap compatibility

The historical Writer count includes only Writer Tasks already materialized
when a saved extension was approved. Later Tasks do not contribute.

### B1

WHEN a saved extension chain follows Fluent's effective-cap rule and its
historical Writer count is supported by the saved Task history,
THE SYSTEM SHALL accept the chain when loading the Work Item.
Test: tests/binary.rs (legacy_round_cap_chain_is_readable)
Test: src/work_model.rs (round_cap_chain_uses_historical_writer_count)

### B2

WHEN an operator inspects supported historical chains through status, Work Item
list, Work Item show, or cleanup preview,
THE SYSTEM SHALL preserve the stored Work history.
Test: tests/binary.rs (legacy_round_cap_observation_preserves_history)

### B3

IF a chain requires a higher starting cap than prior approvals and evidence
allowances establish, and the saved historical Writer count does not support
that cap,
THEN THE SYSTEM SHALL reject the chain with the affected extension identified.
Test: src/work_model.rs (round_cap_chain_rejects_unproven_writer_count)
Test: tests/binary.rs (legacy_round_cap_chain_rejects_altered_history)
Example: Writers created after approval cannot explain a gap in an earlier cap.

### B4

WHEN Fluent records a bounded extension after the materialized Writer count has
exceeded a preceding extension's cap,
THE SYSTEM SHALL write an extension chain that remains valid after reloading.
Test: src/work_model.rs (round_cap_extension_after_writer_overrun_round_trips)
Test: tests/binary.rs (attempt_extend_after_writer_overrun_round_trips)

### B5

WHEN a historical chain becomes readable,
THE SYSTEM SHALL continue to require the existing independent authorization and
evidence checks before recovery or resume.
Test: tests/binary.rs (legacy_round_cap_readability_does_not_authorize_recovery)
Test: tests/binary.rs (attempt_extend_binds_candidate_and_failed_review_bytes)

### B6

WHEN Fluent evaluates cleanup eligibility for Work with a supported historical
chain,
THE SYSTEM SHALL apply the existing terminal-state and workspace-safety rules.
Test: tests/binary.rs (legacy_round_cap_cleanup_preserves_unfinished_work)
Example: A paused unfinished Work Item remains present while eligible terminal
Work in the same isolated fixture can be cleaned.

## Canonical Tester confinement

### B1

WHILE a canonical Tester runs in a linked candidate worktree, THE SYSTEM SHALL
allow configured commands to read that worktree's Git metadata and resolve the
exact candidate repository.
Test: src/tester.rs (tester_sandbox_reads_but_cannot_write_linked_worktree_git_metadata)

### B2

WHILE a canonical Tester runs, THE SYSTEM SHALL deny configured commands write
access to the candidate's common Git metadata.
Test: src/tester.rs (tester_sandbox_reads_but_cannot_write_linked_worktree_git_metadata)

## Actionable final Tester evidence

### B1

IF a required test reference has no passing result while any Tester command
failed, THEN THE SYSTEM SHALL include each failed command, exit code, and durable
stdout and stderr log paths in the corrective evidence sent to the next Writer.
Test: src/candidate_gate.rs (missing_test_reference_reports_failed_tester_command_evidence)

### B2

IF final Tester evidence returns to a corrective Writer, THEN THE SYSTEM SHALL
grant read-only access to the exact gate, structured Tester result, and failed
command logs while leaving sibling Tester artifacts inaccessible.
Test: tests/binary.rs (established_canonical_defects_grant_writer_exact_inputs)
Test: src/work_attempt_loop.rs (final_tester_correction_rejects_noncanonical_log_path)
Test: src/work_task_executor.rs (writer_launch_names_and_confines_preserved_work_item_input)

### B3

IF a corrective Writer declares no candidate change after receiving failed final
Tester command evidence, THEN THE SYSTEM SHALL require its fresh no-change
declaration to name every attached gate, structured result, stdout log, and
stderr log with the SHA-256 digest of the current bytes, and SHALL reject missing,
extra, duplicate, or stale inspection evidence before review or Tester execution.
Test: src/work_task_executor.rs (failed_tester_no_change_requires_every_attached_artifact_digest)
Test: src/work_task_executor.rs (failed_tester_no_change_rejects_stale_artifact_digest)
Test: src/work_task_executor.rs (failed_tester_no_change_accepts_exact_attached_artifact_digests)
Test: src/work_task_executor.rs (failed_tester_no_change_rejects_duplicate_and_extra_artifact_paths)
Test: src/work_task_executor.rs (failed_tester_correction_prompt_requires_digest_bound_inspection)

### B4

IF a corrective Writer preserves the candidate after receiving failed final
Tester command evidence, THEN THE SYSTEM SHALL rerun only the commands whose
prior exit codes were nonzero before launching another Reviewer.
Test: src/work_attempt_loop.rs (unchanged_failed_tester_correction_plans_recheck_before_reviews)
Test: src/work_task_executor.rs (failed_command_recheck_executes_only_the_bound_subset)

An operator MAY submit exact bound host proof to targeted Tests Review before
the local recheck through `attempt evidence attach --required-check`.
Test: tests/binary.rs (canonical_tester_host_evidence_enters_targeted_review)

### B5

WHEN a required command or failed-command recheck fails, THE SYSTEM SHALL keep
its result separate from its diagnosed cause. A verified prerequisite that
prevented execution SHALL produce an environment-recovery handoff. An unexplained
failure SHALL pause for diagnosis without scheduling a source correction.
Established code and harness defects SHALL retain their correction workflows.
Test: tests/binary.rs (ambiguous_check_failure_pauses_without_source_correction)
Test: tests/binary.rs (unavailable_check_environment_does_not_schedule_writer)
Test: src/work_attempt_loop.rs (structured_failure_in_recheck_returns_to_writer)

### B6

IF a failed-command recheck fails, THEN THE SYSTEM SHALL NOT launch a
Reviewer or any unselected Tester command.
Test: src/work_attempt_loop.rs (repeated_unclassified_command_pauses_without_writer)
Test: src/work_task_executor.rs (failed_command_recheck_executes_only_the_bound_subset)

This does not prohibit a separately authorized evidence-targeted Tests Review
of the exact bound canonical failure.
Test: tests/binary.rs (canonical_tester_host_evidence_enters_targeted_review)

### B7

WHEN every selected failed-command recheck passes, THE SYSTEM SHALL continue to
the normal Reviewer round.
Test: src/work_attempt_loop.rs (passing_failed_command_recheck_allows_normal_reviews)

### B8

WHEN Reviewers pass after a failed-command recheck, THE SYSTEM SHALL run the
complete configured final Tester before Learning.
Test: src/work_attempt_loop.rs (reviews_after_passing_recheck_still_plan_full_final_tester)

WHEN the existing complete canonical run plus reviewed replacements already
satisfies every requirement on the unchanged candidate, THE SYSTEM SHALL reuse
that evidence without another complete run.
Test: tests/binary.rs (canonical_tester_recovery_reuses_other_passing_results)

### B9

IF a configured final Tester command fails, THEN THE SYSTEM SHALL retain its
exit code and durable stdout and stderr logs. Verified unmet prerequisites and
ambiguous failures SHALL remain unresolved checks, not established source defects.
Test: src/work_task_executor.rs (execution_context_surfaces_each_failed_tester_command)
Test: src/work_attempt_loop.rs (unclassified_configured_tester_command_preserves_evidence_without_writer)
IF a later no-change Writer preserves that Tester's candidate, THEN THE SYSTEM
SHALL continue to use the exact candidate-bound source Tester as the correction
frontier instead of requiring a new Tester after the no-change Writer.
Test: src/work_attempt_loop.rs (final_tester_correction_preserves_source_before_no_change_writer)

### B10

WHEN a candidate corrected after a failed Tester command reaches tests review,
THE SYSTEM SHALL give the tests reviewer the latest structured Tester result and
failed command logs.
Test: src/work_model.rs (tests_reviewer_receives_latest_tester_correction_evidence)
Test: src/work_attempt_loop.rs (real_corrective_commit_sends_failed_tester_evidence_to_tests_review)

### B11

WHEN a failed-command recheck passes before tests review, THE SYSTEM SHALL
replace the prior failed Tester evidence with the passing recheck result.
Test: src/work_model.rs (passing_recheck_supersedes_failed_tester_review_evidence)
Test: src/work_attempt_loop.rs (passing_failed_command_recheck_allows_normal_reviews)

### B12

IF the final candidate gate's only missing evidence is one command-shaped
`Test:` reference that canonical Tester did not run, and the exact candidate
already has a passing Tests Review, THEN THE SYSTEM SHALL pause before another
Writer and provide an eligible command to attach candidate-bound host evidence
to that rejected gate.
Test: src/work_attempt_loop.rs (missing_command_reference_pauses_for_exact_host_evidence_before_writer)
Test: src/candidate_gate.rs (rejected_gate_does_not_request_ambiguous_or_unrelated_host_evidence)

### B13

WHEN an operator attaches a passing or failing result for the exact requested
command and candidate, THE SYSTEM SHALL preserve immutable gate and evidence
snapshots and send the result to a targeted Tests Review.
Test: src/host_evidence.rs (candidate_gate_evidence_is_candidate_bound_reviewed_and_idempotent)
Test: src/host_evidence.rs (failed_candidate_gate_host_check_reaches_targeted_tests_review)

### B14

WHEN Fluent accepts candidate-gate host evidence, THE SYSTEM SHALL require a
targeted Tests Review before crediting the command.
Test: src/host_evidence.rs (candidate_gate_evidence_is_candidate_bound_reviewed_and_idempotent)

### B15

WHEN the targeted Tests Review accepts candidate-gate host evidence, THE SYSTEM
SHALL credit only its exact command when the candidate gate runs again.
Test: src/host_evidence.rs (candidate_gate_evidence_is_candidate_bound_reviewed_and_idempotent)

### B15a

IF the exact authorized-host command fails because of a candidate defect, THEN
THE SYSTEM SHALL preserve the failed result for targeted Tests Review without
crediting the command as passing evidence, so the reviewer can request a
corrective Writer through the existing evidence disposition.
Test: src/host_evidence.rs (failed_candidate_gate_host_check_reaches_targeted_tests_review)

### B16

WHEN an operator repeats the same candidate-gate evidence attachment, THE
SYSTEM SHALL return the existing recovery without creating another review.
Test: src/host_evidence.rs (candidate_gate_evidence_is_candidate_bound_reviewed_and_idempotent)

### B17

IF the candidate, command, working directory, rejected gate, or related gate
failures do not match the authorized evidence frontier, THEN THE SYSTEM SHALL
reject the attachment without accepting stale evidence.
Test: src/host_evidence.rs (candidate_gate_evidence_is_candidate_bound_reviewed_and_idempotent)
Test: src/candidate_gate.rs (rejected_gate_does_not_request_ambiguous_or_unrelated_host_evidence)

## Persisted status accounting

### B1

WHEN Task execution settles, THE SYSTEM SHALL update that Task's contribution to
one persisted per-Work-Item metrics sidecar without walking unrelated Task
artifact areas.
Test: src/work_status.rs (metrics_are_derived_from_local_work_evidence)

### B2

WHEN `fluent status` or `fluent work-item show` reports cycle-cost metrics, THE
SYSTEM SHALL read the persisted sidecar without recursively walking the Work
Item's artifact tree.
Test: src/work_status.rs (metrics_are_derived_from_local_work_evidence)

### B3

WHEN `fluent work-item rebuild-metrics` runs, THE SYSTEM SHALL rebuild persisted
metrics for the named Work Item, or for every Work Item when no id is supplied.
Test: tests/binary.rs (work_item_rebuild_metrics_indexes_artifacts_for_fast_reads)

### B4

WHEN an executing Task's provider reports a retry delay whose expected resume
time is still in the future, THE SYSTEM SHALL show that Task as `provider-wait`.
Test: src/work_status.rs (executing_task_shows_active_provider_wait)

### B5

WHEN Fluent reports completed cycle-cost metrics, THE SYSTEM SHALL report active
Task duration as total Task duration minus known provider wait.
Test: src/work_status.rs (metrics_are_derived_from_local_work_evidence)

## Carried finding host-evidence handoff

A carried finding is an unresolved Tests finding retained in the current
Writer's immutable corrective assignment. An eligible handoff has a completed
latest Writer, a clean managed candidate, a `needs-user/writer-completion`
pause, settled owners, and no other pending Writer obligation. The assigned
source Review requests one exact command and candidate-relative working directory.

### B1

WHEN an operator explicitly selects a carried finding in an eligible handoff
and attaches matching proof, THE SYSTEM SHALL schedule one targeted Tests Review
against the current candidate for that finding and proof.
Test: tests/binary.rs (carried_finding_evidence_attachment_plans_current_tests_review)
Test: tests/binary.rs (carried_finding_evidence_entrypoint_guidance_and_queue)
Test: tests/binary.rs (carried_finding_evidence_entrypoint_review_inputs)

The original Review continues to name its original candidate. Attachment retains
its bytes, the assignment, historical matrices and Writer output, and starts no
host command, Writer, or Tester. Exact retries retain recovery, evidence, Review
Task, and active dispatch identities, including after interrupted publication.
A later cancellation requires fresh explicit resume after ownership settles.
Test: tests/binary.rs (carried_finding_evidence_retry_preserves_identity_and_stop)
Test: tests/binary.rs (carried_finding_evidence_retry_recovers_interrupted_publication)

### B2

WHEN the targeted Tests Review accepts passing matching proof, THE SYSTEM SHALL
satisfy only the selected carried finding for the bound current candidate.
Failed proof or an unsuccessful Review SHALL leave that finding unsatisfied.
Historical Writer claims, ordinary review domains, canonical Tester commands,
and Learning SHALL retain their existing meaning and gates.
Test: tests/binary.rs (carried_finding_evidence_acceptance_satisfies_only_selected_finding)
Test: tests/binary.rs (carried_finding_evidence_entrypoint_revalidates_accepted_review)
Test: tests/binary.rs (carried_finding_evidence_entrypoint_preserves_later_required_check)
Test: tests/binary.rs (carried_finding_evidence_retry_replaces_failed_or_expired_proof)
Test: tests/binary.rs (carried_finding_evidence_retry_code_change_keeps_source_authority)

### B3

IF the selected finding, assignment, proof, candidate, or execution ownership
fails eligible-handoff validation, THEN THE SYSTEM SHALL reject the handoff
without changing Work or candidate state. Foreground resume, queue admission
and claim, direct Review launch, status, and completion SHALL revalidate the
same source and acceptance bindings.
Test: tests/binary.rs (carried_finding_evidence_attachment_rejects_invalid_bindings)
Test: tests/binary.rs (carried_finding_evidence_entrypoint_rejects_stale_inputs)

Older executables SHALL refuse mutation of finding-bound records and pending
transactions; current readers SHALL preserve supported legacy meaning without
inventing finding acceptance from an absent binding.
Test: tests/binary.rs (carried_finding_evidence_compatibility_refuses_published_records)
Test: tests/binary.rs (carried_finding_evidence_compatibility_refuses_public_transaction)
Test: tests/binary.rs (carried_finding_evidence_compatibility_preserves_legacy_meaning)

## Host evidence recovery

### B1

WHEN an operator attaches a valid schema-version-1 host evidence document to an
eligible Attempt and exact candidate commit, THE SYSTEM SHALL retain immutable
bytes, producer, check, result, timestamp, and digest.
Test: tests/binary.rs (attempt_evidence_attach_preserves_exact_evidence_and_identity)

WHEN preserved artifacts prove every required binding for a supported older
result, THE SYSTEM SHALL apply the same validation and targeted review rules
without requiring a rerun solely because its format is older. Authorized
attachment may publish a deterministic reconstruction audit; observation SHALL
leave the original documents, model, and artifact tree unchanged. Missing
historical applicability facts SHALL remain explicit blockers.
Test: tests/binary.rs (legacy_evidence_reuses_proven_bindings_without_host_rerun)
Test: tests/binary.rs (legacy_evidence_requires_historical_comparison_proof)

### B2

WHEN an operator attaches valid host evidence to an eligible Attempt, THE SYSTEM
SHALL plan the explicitly named failed reviewer roles and no other reviewer roles.
Test: tests/binary.rs (attempt_evidence_attach_plans_targeted_reviews_without_writer)

For an explicitly selected eligible carried Tests finding, THE SYSTEM SHALL
plan one targeted Tests Review against the current candidate while retaining
the source Review at its original candidate.
Test: tests/binary.rs (carried_finding_evidence_attachment_plans_current_tests_review)

For a bound canonical Tester failure, THE SYSTEM SHALL instead plan one
targeted Tests Review without a failed ordinary Review artifact.
Test: tests/binary.rs (canonical_tester_host_evidence_enters_targeted_review)

### B3

WHEN an operator attaches valid host evidence to an eligible Attempt, THE SYSTEM
SHALL add no Writer.
Test: tests/binary.rs (attempt_evidence_attach_plans_targeted_reviews_without_writer)

### B4

WHEN an operator attaches valid host evidence to an eligible Attempt, THE SYSTEM
SHALL add no Tester.
Test: tests/binary.rs (attempt_evidence_attach_plans_targeted_reviews_without_writer)

### B5

WHEN Fluent accepts host evidence, THE SYSTEM SHALL leave the candidate commit
unchanged.
Test: tests/binary.rs (attempt_evidence_attach_preserves_candidate_commit)

### B6

WHEN Fluent accepts host evidence, THE SYSTEM SHALL leave every approved
planning input unchanged.
Test: tests/binary.rs (attempt_evidence_attach_preserves_approved_planning_inputs)

### B7

WHEN Fluent accepts host evidence, THE SYSTEM SHALL keep the unchanged candidate
unlandable until evidence-targeted review and Learning complete.
Test: tests/binary.rs (attempt_evidence_attach_keeps_candidate_unlandable)
Test: src/work_attempt_loop.rs (evidence_targeted_reviews_advance_unchanged_candidate_to_learning)

### B8

IF a host evidence attachment names a stale Work Item, Attempt, candidate,
review artifact, or state owned by a live Task or landing operation, THEN
THE SYSTEM SHALL reject it without changing Work or candidate state. Missing or
changed command, configuration, comparison, declared environment, or required
freshness bindings SHALL exclude a result from satisfying its check while
preserving the original evidence.
Test: tests/binary.rs (attempt_evidence_attach_rejects_stale_or_owned_frontier_without_mutation)
Test: tests/binary.rs (attempt_evidence_attach_rejects_live_task_owner_without_mutation)
Test: tests/binary.rs (attempt_evidence_attach_revalidates_after_landing_owner_releases_lock)
Test: tests/binary.rs (evidence_reuse_rejects_changed_missing_or_expired_bindings)
Test: tests/binary.rs (canonical_completed_acceptance_revalidates_bindings)
Test: tests/binary.rs (requested_completed_acceptance_revalidates_bindings)
Test: tests/binary.rs (independent_review_check_remains_required_after_expiry)
Test: tests/binary.rs (plan_completed_acceptance_revalidates_bindings)
Test: tests/binary.rs (changed_candidate_recovery_runs_fresh_checks)
Test: tests/binary.rs (live_state_evidence_obeys_declared_freshness)
Test: tests/binary.rs (expired_pending_evidence_accepts_fresh_proof)
Test: tests/binary.rs (expired_pending_plan_evidence_accepts_fresh_proof)
Test: tests/binary.rs (expired_pending_requested_evidence_accepts_fresh_proof)
Test: tests/binary.rs (canonical_evidence_rejects_changed_comparison_ref)

For a carried finding, THE SYSTEM SHALL validate the assigned source Review
against its original candidate and matching proof against the current candidate.
Test: tests/binary.rs (carried_finding_evidence_attachment_rejects_invalid_bindings)

### B9

WHEN an evidence-targeted reviewer fails with `Disposition: evidence-needed`
and names exactly one non-empty `External-Condition` and exactly one non-empty
`Additional-Proof`, THE SYSTEM SHALL pause the same Attempt for that further
host evidence without planning a Writer.
Test: src/work_attempt_loop.rs (evidence_needed_review_pauses_without_writer_round)

### B9a

WHEN a complete candidate-bound host check failed and a new evidence-targeted
review contains exactly one `Disposition: evidence-needed` marker with missing,
empty, or otherwise malformed qualification fields, THE SYSTEM SHALL follow the
ordinary `code-change` Writer path.
Test: src/work_attempt_loop.rs (unqualified_evidence_needed_review_plans_ordinary_writer)
Test: src/work_attempt_loop.rs (malformed_single_evidence_needed_review_plans_writer)

### B9b

WHEN a legacy paused recovery contains exactly one unqualified
`Disposition: evidence-needed` marker, including a Plan-row recovery whose
targeted review was persisted with `review_state: not-reviewed`, AND an
operator supplies the explicit candidate, review, evidence-digest, and
resolution authorization through `fluent attempt evidence classify`, THE
SYSTEM SHALL verify the immutable Plan-row binding, record the decision, and
schedule one corrective Writer on the same Attempt with the failed review and
immutable host-evidence snapshot as inputs.
Test: tests/binary.rs (legacy_unqualified_host_evidence_can_be_classified_for_code_change)
Test: src/host_evidence.rs (plan_row_failed_review_with_legacy_not_reviewed_state_can_start_correction)

### B9c

WHEN an operator repeats the exact candidate-bound classification request, THE
SYSTEM SHALL return the stored Writer and audit result without creating another
Task, audit, or evidence record.
Test: tests/binary.rs (failed_host_evidence_can_be_classified_for_code_change)
Test: src/work_model.rs (host_evidence_classification_is_idempotent)

### B9d

IF a candidate-bound classification's review, evidence, candidate, Attempt,
worktree, or authorization no longer matches its persisted frontier, THEN THE
SYSTEM SHALL reject the request without mutation.
Test: tests/binary.rs (host_evidence_classification_rejects_stale_inputs)
Test: tests/binary.rs (classified_host_evidence_queue_resume_rejects_stale_continuation)

### B10

WHEN an evidence-targeted reviewer fails with `Disposition: code-change`, THE
SYSTEM SHALL resume the ordinary Writer path with that finding.
Test: src/work_attempt_loop.rs (code_change_evidence_review_plans_ordinary_writer)

The evidence-backed Writer allowance applies only after the targeted review
records `code-change` and its finding becomes a Writer input. Reviewing,
`evidence-needed`, and completed recovery records do not increase the cap.

### B11

WHEN an evidence-targeted reviewer passes, THE SYSTEM SHALL preserve the prior
Writer history.
Test: src/work_attempt_loop.rs (evidence_targeted_review_pass_preserves_writer_and_tester)

### B11a

WHEN the only failed review requests one well-formed focused Tests Review check,
THE SYSTEM SHALL pause with the exact evidence-run command without planning a
Writer.
Test: src/work_attempt_loop.rs (ordinary_tests_review_focused_request_pauses_without_writer_round)
Test: src/work_status.rs (focused_evidence_status_names_exact_run_command)

### B11b

WHEN an operator runs one accepted focused evidence check for a failed Tests
Review, THE SYSTEM SHALL retain its exact command, resolved working directory,
captured result, output, and candidate commit as host evidence.
Test: tests/binary.rs (attempt_evidence_run_plans_only_targeted_tests_review)
Test: tests/binary.rs (attempt_evidence_run_records_timeout_as_failed_evidence)

### B11c

WHEN Fluent accepts a focused evidence result, THE SYSTEM SHALL plan no Writer.
Test: tests/binary.rs (attempt_evidence_run_plans_only_targeted_tests_review)

### B11d

WHILE a focused evidence check runs, THE SYSTEM SHALL keep candidate source
read-only.
Test: tests/binary.rs (scheduled_verification_task_runs_in_confined_worker)

### B11e

WHILE a focused evidence check runs, THE SYSTEM SHALL provide a private writable
scratch path for build and cache state.
Test: tests/binary.rs (scheduled_verification_task_runs_in_confined_worker)

### B11h

WHILE a focused evidence check runs in a linked candidate worktree, THE SYSTEM
SHALL allow the check to read that worktree's Git metadata.
Test: src/host_evidence.rs (focused_evidence_includes_linked_git_metadata_in_read_and_deny_boundaries)

### B11i

WHILE a focused evidence check runs in a linked candidate worktree, THE SYSTEM
SHALL deny writes to that worktree's Git metadata.
Test: src/host_evidence.rs (focused_evidence_includes_linked_git_metadata_in_read_and_deny_boundaries)

### B11f

WHILE a focused evidence check runs, THE SYSTEM SHALL withhold provider
credential environment variables and files under the caller's Claude, Codex,
and agent state directories from the child command.
Test: tests/binary.rs (scheduled_verification_task_runs_in_confined_worker)

### B11g

IF a focused evidence request is missing, malformed, or belongs to a role other
than Tests Review, THEN THE SYSTEM SHALL reject it without changing the Work
Item or candidate.
Test: tests/binary.rs (attempt_evidence_run_rejects_invalid_request_without_mutation)
Test: tests/binary.rs (attempt_evidence_run_rejects_non_tests_review_without_mutation)
Test: tests/binary.rs (attempt_evidence_run_rejects_legacy_review_without_explicit_request)

### B12

WHEN an evidence-targeted reviewer passes, THE SYSTEM SHALL preserve the prior
Tester history.
Test: src/work_attempt_loop.rs (evidence_targeted_review_pass_preserves_writer_and_tester)

### B13

WHEN an evidence-targeted reviewer passes, THE SYSTEM SHALL complete that
recovery frontier. A carried-finding recovery additionally requires passing
proof and satisfies only its selected finding in current completion assessment.
It SHALL preserve the historical Writer's claims.
Test: src/work_attempt_loop.rs (evidence_targeted_review_pass_preserves_writer_and_tester)
Test: tests/binary.rs (carried_finding_evidence_acceptance_satisfies_only_selected_finding)

### B14

IF a failed Attempt ends in a no-output Writer whose exact inputs are the failed
review artifacts named by the attachment, THEN THE SYSTEM SHALL recover in that
same Attempt.
Test: tests/binary.rs (attempt_evidence_attach_recovers_failed_no_change_writer_in_same_attempt)

### B14a

IF an evidence-targeted Review requests a code change, its exact corrective
Writer is interrupted before producing output, and the candidate remains clean
and unchanged, THEN THE SYSTEM SHALL allow replacement evidence for that Review,
preserve the Writer as superseded history, and resume only the targeted Review.
Test: tests/binary.rs (attempt_evidence_attach_supersedes_interrupted_output_free_evidence_correction)

### B15

IF a failed Attempt does not end in a no-output Writer whose exact inputs are the
failed review artifacts named by the attachment and does not meet the explicit
carried-finding handoff requirements, THEN THE SYSTEM SHALL reject the attachment
without mutation.
Test: tests/binary.rs (attempt_evidence_attach_rejects_ineligible_failed_attempt)

### B16

WHEN an evidence-only recovery needs more host evidence, THE SYSTEM SHALL show
its recovery history and the exact next attachment command.
Test: tests/binary.rs (work_show_explains_evidence_only_recovery_history_and_next_action)
For a required check, THE SYSTEM SHALL report its exact identity, unresolved
reason, and an eligible attachment or resume command. If current proof or
ownership prevents an operation, it SHALL identify that missing proof or
settlement instead. Foreground admission, queue admission and claim SHALL
revalidate the same bindings after read-only guidance.
Test: tests/binary.rs (evidence_recovery_guidance_matches_foreground_and_queue_admission)

### B17

WHEN an evidence-targeted reviewer fails, THE SYSTEM SHALL accept only the typed
dispositions `evidence-needed` and `code-change`.
Test: src/review.rs (evidence_needed_requires_actionable_host_reason)

### B18

WHEN one unexplained failed-command recheck reaches the Writer-round cap, THE
SYSTEM SHALL show a candidate-bound evidence attachment command that accepts a
passing or failing authorized-host result and plans a targeted Tests Review. A
passing result SHALL name the exact failed command; a failing result MAY name a
related direct command as diagnostic evidence, but SHALL remain bound to the
exact recheck and SHALL NOT satisfy it.
Test: src/work_attempt_loop.rs (external_recheck_evidence_advances_round_cap_to_candidate)
Test: src/work_attempt_loop.rs (failed_recheck_host_evidence_can_request_corrective_writer)
Test: src/work_attempt_loop.rs (failed_related_host_diagnostic_preserves_recheck_identity)

A bound canonical Tester failure SHALL admit the same exact-command evidence
path before a recheck or exhausted Writer cap, including when later targeted
Plan review already passed.
Test: tests/binary.rs (canonical_tester_host_evidence_enters_targeted_review)

### B19

WHEN a targeted Tests Review accepts passing evidence for the unchanged
candidate and exact failed command, THE SYSTEM SHALL retain that command's
accepted result and evaluate the remaining gates without rerunning it.
Test: src/work_attempt_loop.rs (external_recheck_evidence_advances_round_cap_to_candidate)

Advancement SHALL require the complete canonical run and accepted replacements
to satisfy every outstanding Tester check and retain the other existing gates.
Test: tests/binary.rs (canonical_tester_recovery_reuses_other_passing_results)
Test: tests/binary.rs (canonical_tester_recovery_preserves_aggregate_blockers)

WHEN a completed Approach-recovery Attempt resumes, THE SYSTEM SHALL reassess
the current canonical verification before reporting its existing Merge
Candidate ready. A satisfied assessment SHALL preserve supported accepted host
proof and applicable pre-existing baseline treatment without restarting a
completed Task.
Test: tests/binary.rs (round_cap_approach_revision_completed_resume_accepts_satisfied_verification)
Test: tests/binary.rs (round_cap_approach_revision_completed_resume_accepts_failed_command_with_host_proof)

### B19a

WHEN an authorized-host run exposes a candidate defect for the exact unchanged
candidate and failed-command recheck, or through a related direct diagnostic
command, THE SYSTEM SHALL preserve that failed evidence and let a targeted
Tests Review request a corrective Writer without treating the diagnostic as a
passing recheck.
Test: src/work_attempt_loop.rs (failed_recheck_host_evidence_can_request_corrective_writer)
Test: src/work_attempt_loop.rs (failed_related_host_diagnostic_preserves_recheck_identity)

### B20

IF attached proof is stale, mismatched, unverifiable, or contradictory, THEN THE
SYSTEM SHALL exclude it from satisfying the current check. Reviewed proof for
one exact canonical command SHALL credit only that occurrence while independent
failures remain blockers. A related diagnostic command SHALL NOT substitute for
the required command; the existing failing-diagnostic recheck path remains
available to request a correction.
Test: tests/binary.rs (reviewed_host_result_satisfies_only_bound_check)
Test: tests/binary.rs (canonical_tester_rejects_diagnostic_command_without_mutation)
Test: src/work_attempt_loop.rs (passing_related_host_diagnostic_is_rejected_with_exact_command_guidance)

IF a completed Approach-recovery Attempt's current final Tester result has an
unresolved command failure, THE SYSTEM SHALL reject foreground resume even
when the result has no structured test failure, a consistent summary, no
harness error, and the saved candidate gate passed. The rejection SHALL leave
the candidate, Work history, and evidence unchanged.
Test: tests/binary.rs (round_cap_approach_revision_completed_resume_rejects_unresolved_command)

### B21

WHEN a failed-command recheck cannot use command-only evidence recovery, THE
SYSTEM SHALL retain an extension frontier bound to the unchanged candidate and
recheck artifacts.
Test: src/work_attempt_loop.rs (failed_recheck_is_a_round_cap_extension_frontier)

### B22

WHEN a rejected final-Tester gate remains unresolved after a no-change Writer,
THE SYSTEM SHALL retain that exact candidate-bound gate, Tester result, and
failed-command logs as an eligible round-cap extension frontier.
Test: src/work_attempt_loop.rs (rejected_final_tester_gate_remains_round_cap_frontier_after_no_change_writer)

### B23

IF Writer completion is paused only because a required Plan row names an
authorized-host command, THEN THE SYSTEM SHALL identify that row, exact command,
candidate, and completion matrix in the handoff and provide a candidate-bound
`--plan-row` evidence attachment transition instead of requiring a Writer
commit.
Test: src/host_evidence.rs (plan_row_host_evidence_binds_unchanged_candidate_and_is_idempotent)

### B24

WHEN an operator attaches a schema-version-1 host evidence document for the
exact pending Plan row and unchanged candidate, THE SYSTEM SHALL record an
audited binding to the Work Item, Attempt, Writer, source matrix, evidence
digest, row, and command, then create exactly one targeted Tests Review.
Test: src/host_evidence.rs (plan_row_host_evidence_binds_unchanged_candidate_and_is_idempotent)

### B25

WHEN the authorized-host Plan-row evidence and targeted Tests Review pass, THE
SYSTEM SHALL credit the host-owned row without rewriting the Writer matrix or
creating a source commit, and SHALL require complete ordinary review coverage
before continuing through the final Tester and Learner gates. Targeted evidence
review SHALL NOT supply ordinary coverage. A failing result SHALL remain uncredited
for a corrective review. Repeating the exact attachment SHALL return the existing recovery;
stale candidates, commands, or rows SHALL be rejected without mutation.
Test: src/host_evidence.rs (plan_row_host_evidence_binds_unchanged_candidate_and_is_idempotent)
Test: src/host_evidence.rs (plan_row_host_evidence_rejects_wrong_command_without_mutation)

A later no-change Writer or unrelated Task SHALL NOT invalidate accepted proof
when the candidate and all check bindings still match and no newer conflicting
result remains unresolved. Original Writer, matrix,
result, and targeted Review identities remain provenance. Candidate-code checks
SHALL NOT expire solely because of age; live-state checks require an explicit
valid expiry and become inapplicable at that boundary.
Test: tests/binary.rs (accepted_evidence_survives_no_change_writer_and_resume)
Test: tests/binary.rs (candidate_code_evidence_does_not_expire_by_age)
Test: src/recovery_contracts.rs (evidence_freshness_uses_exact_expiry_and_requires_a_policy)

### B26

WHEN a required Plan row places one executable host command in its direct
reference, inline code, Deliverable, or Verification prose, THE SYSTEM SHALL
resolve that command without changing the approved planning text and use it for
the candidate-bound evidence check. IF the row contains no command or more than
one command-shaped reference, THE SYSTEM SHALL keep the Attempt paused for an
explicit human decision rather than selecting one implicitly.
Test: src/host_evidence.rs (plan_row_host_evidence_resolves_command_from_deliverable_prose)
Test: src/host_evidence.rs (plan_row_host_evidence_resolves_inline_command_and_rejects_ambiguity)

## Test harnesses

| Harness | Runs | Usage |
|---|---|---|
| `tests/test-skill` | Skill conversation simulations | `tests/test-skill <scenario> <skill> [--judge]` |

### B1

WHEN `tests/test-skill` completes a skill conversation simulation,
THE HARNESS SHALL print the run directory and list `transcript.md` as
the full conversation artifact. The harness SHALL list `brief.md` only
when it extracted a captured artifact from the skill agent response, and
SHALL list `verdict.md` only when the judge wrote scoring.
Test: tests/behaviors/operations/test-skill-harness-artifacts.sh

---

## Version reporting

### B1

WHEN `fluent version` is invoked,
THE SYSTEM SHALL print the Fluent package version and build commit to
stdout and exit successfully without requiring a Fluent project.
Test: tests/binary.rs (version_prints_package_version_and_commit)
Test: tests/behaviors/operations/test-version.sh

### B2

WHEN `fluent --version` is invoked,
THE SYSTEM SHALL print the same version and build commit output as
`fluent version`.
Test: tests/binary.rs (version_flag_matches_version_subcommand)

## Work-model compatibility

### B1

WHEN a stored Attempt contains `host-sandbox`, `tester-harness`, or an unknown
future pause-kind string, THE SYSTEM SHALL deserialize the Work Item without
discarding or rewriting that string.
Test: src/work_model.rs (known_and_future_pause_kinds_preserve_their_wire_values)
Test: src/work_model.rs (host_sandbox_pause_round_trips_through_storage)
Test: src/work_model.rs (tester_harness_pause_round_trips_through_storage)
Test: src/work_model.rs (unknown_pause_is_readable_but_mutation_fails_closed)

WHEN current Fluent writes or recovers a split Work Item or pending journal,
THE SYSTEM SHALL preserve required-check sources, applicability, reconstruction
audit references, and accepted Review digests.
Test: tests/behaviors/operations/test-evidence-recovery-old-reader.sh

WHEN current Fluent writes or recovers an assignment-bearing split Work Item or
pending journal, THE SYSTEM SHALL preserve the host-owned corrective Writer
assignment.
Test: tests/binary.rs (corrective_writer_assignment_survives_candidate_change)
Test: tests/binary.rs (work_attempt_run_plans_followup_for_failed_reviews)
Test: src/work_model.rs (writer_assignment_safety_retries_interrupted_publication)

IF a pinned pre-change executable reads a Work Item record or pending journal
whose recognized outer or inner schema is newer than it supports, THEN THE
SYSTEM SHALL refuse public mutation before changing any model or artifact
bytes.
Test: tests/binary.rs (writer_assignment_old_reader_refuses_published_records)
Test: tests/binary.rs (writer_assignment_old_reader_refuses_pending_transactions)
Test: tests/binary.rs (writer_assignment_old_reader_reaches_inner_fences)

### B2

WHEN a stored Attempt contains an unknown future pause-kind string, THE SYSTEM
SHALL display the state through read-only status, list, and show commands with an
upgrade warning.
Test: src/work_status.rs (unknown_pause_is_visible_with_upgrade_warning)
Test: tests/binary.rs (read_only_commands_show_future_pause_but_attempt_run_fails_closed)

### B3

WHEN a Work Item contains an unknown pause kind, THE SYSTEM SHALL reject Task,
Attempt, landing, model, and cleanup mutations before changing that Work Item or
its candidate, while retaining read-only inspection.
Test: src/work_model.rs (unknown_pause_is_readable_but_mutation_fails_closed)
Test: src/cleanup.rs (unknown_pause_work_item_is_not_a_cleanup_candidate)
Test: tests/binary.rs (read_only_commands_show_future_pause_but_attempt_run_fails_closed)

### B4

WHEN Fluent writes a Work Item record, THE SYSTEM SHALL record the current Fluent
package version as its model writer version.
Test: src/work_model.rs (attempt_pause_kind_round_trips_through_storage)

### B5

IF an installed binary reads a Work Item written by a newer semantic version,
THEN read-only commands SHALL display an upgrade warning.
Test: src/work_model.rs (newer_model_writer_version_warns_and_blocks_mutation)
Test: tests/binary.rs (read_only_commands_warn_when_model_writer_is_newer)

### B6

IF an installed binary tries to mutate a Work Item written by a newer semantic
version, THEN THE SYSTEM SHALL reject the mutation until Fluent is upgraded.
Test: src/work_model.rs (newer_model_writer_version_warns_and_blocks_mutation)

## Planning and release guardrails

### B1

WHEN Work Item intake receives approved planning context, THE SYSTEM SHALL use
the existing behavior and Plan parsers to count approved behaviors and required
Plan rows, use the larger count as the deterministic scope breadth, and compare
it with the layered `planning.scope-limit` configuration.
Test: src/config.rs (planning_scope_limit_resolves_from_project_and_rejects_zero)
Test: src/work_model.rs (planning_scope_counts_behaviors_and_required_plan_rows)

### B2

IF behavior or Plan breadth exceeds the configured scope reference and the
operator has not passed `--authorize-large-scope`, THEN THE SYSTEM SHALL reject
Work creation without writing Work state, report both counts and the configured
source, and report the minimum policy groups while stating that this arithmetic
minimum is not an optimal Work Item count.
Test: tests/binary.rs (large_scope_requires_explicit_authorization_and_records_diagnostic)
Test: tests/binary.rs (behavior_breadth_requires_scope_authorization_without_extra_plan_detail)

### B3

WHEN the operator explicitly authorizes planning context above the configured
scope reference, THE SYSTEM SHALL persist the scope diagnostic and authorization.
Test: tests/binary.rs (large_scope_requires_explicit_authorization_and_records_diagnostic)
Test: tests/binary.rs (behavior_breadth_requires_scope_authorization_without_extra_plan_detail)

### B4

WHEN a release Work Item is created, THE SYSTEM SHALL require and persist one or
more accepted release criteria before execution can begin.
Test: tests/binary.rs (release_findings_default_to_follow_up_and_blockers_require_criteria)

### B5

WHEN an operator classifies a finding for release Work, THE SYSTEM SHALL keep it
as a proposed follow-up by default.
Test: src/work_model.rs (release_findings_fail_closed_unless_mapped_to_accepted_criteria)
Test: tests/binary.rs (release_findings_default_to_follow_up_and_blockers_require_criteria)

### B6

IF the operator classifies a finding as a release blocker, THEN THE SYSTEM SHALL
require an exact mapping to an accepted release criterion and reject an unknown
mapping without recording the finding.
Test: src/work_model.rs (release_findings_fail_closed_unless_mapped_to_accepted_criteria)
Test: tests/binary.rs (release_findings_default_to_follow_up_and_blockers_require_criteria)

### B7

IF fewer than five current-calibration code Attempts between half and twice the
proposed breadth have completed with passing reviews and successful Learning,
THEN THE SYSTEM SHALL report that project-local scope evidence is not calibrated
and include the eligible and comparable Attempt counts.
Test: src/scope_calibration.rs (local_scope_evidence_excludes_old_distant_and_unsuccessful_work)
Test: src/scope_calibration.rs (local_scope_evidence_requires_five_nearby_successful_versioned_attempts)

### B8

WHEN at least five current-calibration code Attempts between half and twice the
proposed breadth have completed with passing reviews and successful Learning,
THE SYSTEM SHALL report the project-local one-pass rate, median reviewed Writer
rounds, and the exact comparison band, with pre-review Writer continuations
counted outside reviewed rounds.
Test: src/scope_calibration.rs (local_scope_evidence_requires_five_nearby_successful_versioned_attempts)
Test: src/scope_calibration.rs (pre_review_continuations_do_not_count_as_corrective_rounds)

### B9

WHEN project-local scope evidence is calibrated, THE SYSTEM SHALL report median
recorded active task time with its timed Attempt count, or report duration as
unavailable when no eligible Attempt has timestamps.
Test: src/scope_calibration.rs (local_scope_evidence_requires_five_nearby_successful_versioned_attempts)
Test: src/scope_calibration.rs (calibrated_evidence_reports_unavailable_when_task_durations_are_missing)

### B10

IF a Work Item record cannot be read while gathering project-local scope
evidence, THEN THE SYSTEM SHALL omit that record from calibration and report the
number of omitted records.
Test: tests/binary.rs (scope_evidence_reports_omitted_unreadable_work_items)

### B11

WHEN project-local scope evidence is reported, THE SYSTEM SHALL leave the
layered configured scope reference unchanged.
Test: tests/binary.rs (behavior_breadth_requires_scope_authorization_without_extra_plan_detail)

## Observations management

### B1

WHEN `fluent observation create "<content>"` is invoked,
THE SYSTEM SHALL write a new observation file at
`.fluent/observations/<id>.md` where `<id>` follows the format
`YYYYMMDD-HHMMSS-<short-title>` derived from the current timestamp
and a sanitized kebab form of the content's first line, and SHALL
print the generated `<id>` on stdout.
Test: tests/binary.rs (observations_add_with_inline_content)
Test: src/observations.rs (generate_id_includes_timestamp_and_slug)

### B2

WHEN `fluent observation create` is invoked without an inline content
argument,
THE SYSTEM SHALL read the observation body from stdin and SHALL fail
with a clear error if stdin is empty.
Test: tests/binary.rs (observations_add_from_stdin)
Test: tests/binary.rs (observations_add_empty_stdin_errors)

### B3

WHEN two `fluent observation create` invocations would generate the
same `<id>` (same second, same first-line title),
THE SYSTEM SHALL suffix the second with a counter
(`YYYYMMDD-HHMMSS-<short-title>-2`) so the resulting filenames are
unique.
Test: src/observations.rs (resolve_collision_sequential_suffixes)
Test: src/observations.rs (migrate_collision_suffixes)

### B4

WHEN `fluent observation resolve <id> "<resolution>"` is invoked
and `<id>` matches exactly one file under `.fluent/observations/`,
THE SYSTEM SHALL append the resolution context to the file
(preserving the existing observation content) and move the file to
`.fluent/observations/resolved/<id>.md`.
Test: tests/binary.rs (observations_resolve_inline)

### B5

WHEN `fluent observation resolve <id>` is invoked without an inline
resolution argument,
THE SYSTEM SHALL read the resolution body from stdin and SHALL fail
with a clear error if stdin is empty.
Untestable: Requires interactive stdin pipe not supported by current binary test harness

### B6

IF `<id>` matches zero open observation files when resolving,
THEN THE SYSTEM SHALL exit non-zero with an error naming the missing
id.
Test: tests/binary.rs (observations_resolve_unknown_id_errors)

### B7

IF `<id>` is supplied as a unique prefix of exactly one open
observation id,
THEN THE SYSTEM SHALL expand it to the full id for `resolve` and
`show`.
Test: tests/binary.rs (observations_resolve_prefix_unique_match)
Test: src/observations.rs (expand_prefix_unique_match)

### B8

IF `<id>` is supplied as a prefix that matches multiple ids,
THEN THE SYSTEM SHALL exit non-zero, list the matches, and ask the
user to disambiguate.
Test: tests/binary.rs (observations_resolve_prefix_ambiguous_errors)
Test: src/observations.rs (expand_prefix_ambiguous)

### B9

WHEN `fluent observation list` is invoked,
THE SYSTEM SHALL print one line per open observation under
`.fluent/observations/`, ordered by id ascending (chronological), in
the format `<id>  <first line of body>`.
Test: tests/binary.rs (observations_list_orders_chronologically)

### B10

WHEN `fluent observation show <id>` is invoked,
THE SYSTEM SHALL print the body of the observation at
`.fluent/observations/<id>.md` if present, otherwise at
`.fluent/observations/resolved/<id>.md`, on stdout.
Test: tests/binary.rs (observations_show_open_and_resolved)

### B11

WHEN `fluent observation migrate` is invoked with monolithic
observation files present,
THE SYSTEM SHALL split `.fluent/observations.md` and
`.fluent/observations-resolved.md` into one file per observation
under `.fluent/observations/<id>.md` and
`.fluent/observations/resolved/<id>.md` respectively, preserving
each observation's content verbatim, and remove the monolithic files.
Test: tests/binary.rs (observations_migrate_splits_monolithic_files)
Test: src/observations.rs (migrate_splits_and_removes_monolithic)
Test: src/observations.rs (migrate_idempotent)

### B12

WHEN `fluent observation migrate` is invoked with no monolithic
observation files present,
THE SYSTEM SHALL exit successfully without creating or modifying files.
Test: tests/binary.rs (observations_migrate_splits_monolithic_files)
Test: src/observations.rs (migrate_idempotent)

---

## Work Item input artifacts

### B1

WHEN `fluent work-item create` receives one or more repeatable
`--input-artifact <path>` arguments that name existing managed artifact or
progress files,
THE SYSTEM SHALL preserve their exact bytes as immutable Work Item-owned inputs
and record each canonical source path, snapshot path, and SHA-256 digest.
Test: tests/binary.rs (work_create_snapshots_exact_managed_input_artifact_bytes)
Test: tests/binary.rs (concurrent_same_id_work_creation_preserves_the_winners_input_snapshot)
Test: src/work_model.rs (post_publication_creation_error_retains_recoverable_input_snapshot)

### B2

WHEN a Work Item has preserved input artifacts,
THE SYSTEM SHALL expose their durable identities through
`fluent work-item show <id>`.
Test: tests/binary.rs (work_show_reports_preserved_work_item_input_artifacts)

### B3a

WHEN Fluent creates any initial or follow-up Writer Task for a Work Item with
preserved inputs,
THE SYSTEM SHALL declare each preserved snapshot once in the Task inputs and
name it as authoritative input in the Writer prompt.
Test: tests/binary.rs (every_writer_receives_preserved_work_item_input_artifacts_read_only)
Test: src/work_task_executor.rs (initial_writer_prompt_names_preserved_work_item_inputs)

### B3b

WHEN Fluent launches a Writer with preserved Work Item inputs,
THE SYSTEM SHALL grant the Writer read-only access to each input's isolated
managed directory.
Test: src/work_task_executor.rs (writer_launch_names_and_confines_preserved_work_item_input)

### B4

WHEN Fluent creates a reviewer Task for a Work Item with preserved inputs,
THE SYSTEM SHALL combine those snapshots with the Tester, progress, and
role-matched prior-review inputs that apply to that reviewer.
Test: tests/binary.rs (reviewers_receive_preserved_work_item_inputs_and_attempt_inputs)

### B5

IF a requested Work Item input is missing, is not a regular file, escapes the
managed artifact and progress roots, or cannot be installed completely,
THEN THE SYSTEM SHALL reject Work Item creation without storing the Work Item
or a partial input snapshot tree.
Test: tests/binary.rs (work_create_rejects_invalid_input_artifact_before_persisting)
Test: tests/binary.rs (work_create_storage_rejection_leaves_no_input_snapshot)

---

## Work Item intake and inspection

### B1

WHEN two threads create, read, and write distinct Work Items
through the same `WorkModelStore` instance,
THE SYSTEM SHALL keep each Work Item's split-storage records
consistent — no thread observes another's state, and each Work
Item's items/, attempts/, tasks/, merge-candidates/, and artifacts/
paths are written without race or partial state.
Test: src/work_model.rs (concurrent_writes_to_distinct_work_items_do_not_race)

### B2

WHEN `fluent work-item create <id> --title <title>` is invoked from a
directory,
THE SYSTEM SHALL create `.fluent/work/items/<id>.json` containing Work
Item metadata with that id and title, while Attempts, Tasks, and Merge
Candidates remain in their split collections.
Test: tests/binary.rs (work_create_writes_minimal_work_item)
Test: tests/behaviors/operations/test-work-inspection.sh (work create writes minimal Work Item)

### B3

IF `fluent work-item create <id> --title <title>` is invoked for an
existing Work Item id,
THEN THE SYSTEM SHALL exit non-zero and leave the existing Work Item
unchanged.
Test: tests/binary.rs (work_create_refuses_existing_work_item)
Test: tests/behaviors/operations/test-work-inspection.sh (work create existing item fails)

### B4

IF `fluent work-item create <id> --title <title>` is invoked with an invalid
Work Item id,
THEN THE SYSTEM SHALL exit non-zero and not write a Work Item file.
Test: tests/binary.rs (work_create_rejects_invalid_work_item_id)
Test: tests/behaviors/operations/test-work-inspection.sh (work create invalid id fails)

### B5

WHEN a Work Item is created through intake,
THE SYSTEM SHALL make it visible through `fluent work-item list` and
`fluent work-item show <id>`.
Test: tests/binary.rs (work_create_item_is_visible_through_list_and_show)
Test: tests/behaviors/operations/test-work-inspection.sh (work create item is visible)

### B6

WHEN `fluent work-item create` is invoked with rich instructions,
THE SYSTEM SHALL persist those instructions in stored Work Item state and
make them visible through `fluent work-item show <id>`.
Test: tests/binary.rs (work_create_persists_instructions_and_attempt_copies_them_to_write_task)
Test: tests/behaviors/operations/test-work-inspection.sh (work create persists instructions)

### B7

WHEN `fluent work-item create` is invoked with approved planning context,
THE SYSTEM SHALL persist the brief, behaviors, approach, and plan context
in stored Work Item state and make that context visible through
`fluent work-item show <id>`.
Test: tests/binary.rs (work_create_persists_planning_context_and_attempt_copies_it_to_write_task)

### B8

WHEN planning skills describe how to pass approved planning context to
delegated Work execution,
THE SYSTEM SHALL describe Work Item planning context through
`fluent work-item create --brief-file --behaviors-file --approach-file
--plan-file` as the path for storing approved planning context.
Test: tests/behaviors/operations/test-planning-skills-work-context.sh

### B9

WHEN `fluent work-item list` is invoked,
THE SYSTEM SHALL read stored Work Items from `.fluent/work/items/` and
assemble Attempts, Tasks, and Merge Candidates only from their split
collections before printing each Work Item with its id and title.
Test: tests/binary.rs (work_list_outputs_stored_work_items)
Test: tests/behaviors/operations/test-work-inspection.sh (work list prints stored Work Items)

### B10

IF a `.fluent/work/items/<id>.json` file contains nested Attempts,
Tasks, or Merge Candidates,
THEN THE SYSTEM SHALL ignore those nested operational collections and
expose only split-collection Attempt, Task, and Merge Candidate records.
Test: tests/work_model_external.rs (work_model_store_ignores_nested_operational_collections)

### B11

WHEN `fluent work-item list` is invoked and no Work Items exist,
THE SYSTEM SHALL print an empty-state message and exit successfully.
Test: tests/binary.rs (work_list_empty_state_succeeds_without_work_items)
Test: tests/behaviors/operations/test-work-inspection.sh (work list prints empty state)

### B12

WHEN `fluent work-item show <id>` is invoked for a stored Work Item,
THE SYSTEM SHALL print the Work Item as deterministic pretty JSON.
Test: tests/binary.rs (work_show_outputs_pretty_json_for_one_work_item)
Test: tests/behaviors/operations/test-work-inspection.sh (work show prints pretty JSON)

### B13

IF `fluent work-item show <id>` is invoked for a missing Work Item,
THEN THE SYSTEM SHALL exit non-zero and report that the Work Item was
not found.
Test: tests/binary.rs (work_show_missing_item_reports_not_found)
Test: tests/behaviors/operations/test-work-inspection.sh (work show missing item fails)

### B14

WHEN `fluent work-item abandon <id>` is invoked for a stored Work Item
without executing or reviewing Attempts, executing Tasks, reviewing
Merge Candidates, or executing Merge Candidate merges,
THE SYSTEM SHALL record durable Work Item abandonment state and persist
the supplied reason when one is provided.
Test: src/work_model.rs (abandon_records_reason_on_inactive_work_item)
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon persists reason)

### B15

IF `fluent work-item abandon <id>` is invoked for a missing Work Item,
THEN THE SYSTEM SHALL exit non-zero, report that the Work Item was not
found, and leave Work state unchanged.
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon missing item fails)

### B16

IF `fluent work-item abandon <id>` is invoked for a Work Item with an
executing or reviewing Attempt, an executing Task held by a live
runner's lease, a reviewing Merge Candidate, or an executing Merge
Candidate merge,
THEN THE SYSTEM SHALL exit non-zero and leave Work Item state unchanged.
Test: src/work_model.rs (abandon_rejects_executing_attempt_without_changing_marker)
Test: src/work_model.rs (abandon_rejects_reviewing_attempt_without_changing_marker)
Test: src/work_model.rs (abandon_rejects_executing_task_without_changing_marker)
Test: src/work_model.rs (abandon_rejects_active_merge_candidate_without_changing_marker)
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon active item fails without state change)
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon reviewing attempt fails without state change)
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon executing task fails without state change)
Test: tests/behaviors/operations/test-work-inspection.sh (work abandon active merge candidate fails without state change)

### B17

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

### B18

IF stored Work Item state contains invalid JSON, an invalid id, or a
model validation error,
THEN THE SYSTEM SHALL exit non-zero and report the invalid file or
object.
Test: tests/binary.rs (work_list_reports_invalid_stored_json_path)
Test: tests/binary.rs (work_list_reports_stored_work_item_id_mismatch)
Test: tests/binary.rs (work_list_reports_invalid_stored_work_item_id)
Test: tests/binary.rs (work_list_reports_invalid_stored_model)
Test: tests/behaviors/operations/test-work-inspection.sh (work list reports invalid stored state)

### B19

WHEN Work Items are stored,
THE SYSTEM SHALL keep Work Item intake and inspection independent from
other Fluent state.
Test: tests/binary.rs (work_list_empty_state_succeeds_without_work_items)

### B20

WHEN `fluent attempt create <work-item-id>` is invoked without an
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

### B21

WHEN `fluent attempt create <work-item-id> <attempt-id>` is invoked with
an explicit attempt-id,
THE SYSTEM SHALL create the Attempt with that exact id.
Test: tests/binary.rs (work_attempt_adds_planned_attempt_with_initial_write_task)
Test: tests/binary.rs (work_attempt_explicit_id_still_works)

### B22

WHEN `fluent attempt run <work-item-id>` is invoked without an
attempt-id,
THE SYSTEM SHALL operate on the most recently created Attempt of the
Work Item.
Test: src/work_model.rs (latest_attempt_id_returns_last)

### B23

WHEN `fluent attempt run <work-item-id> <attempt-id>` is invoked
with an explicit attempt-id,
THE SYSTEM SHALL operate on that exact Attempt.
Test: tests/binary.rs (work_attempt_explicit_id_still_works)

### B24

WHEN `fluent merge-candidate land <work-item-id>` is invoked without a
merge-candidate-id,
THE SYSTEM SHALL operate on the most recently created Merge Candidate
of the Work Item.
Test: src/work_model.rs (latest_merge_candidate_id_returns_last)

### B25

WHEN `fluent merge-candidate land <work-item-id> <merge-candidate-id>` is
invoked with an explicit merge-candidate-id,
THE SYSTEM SHALL use that exact Merge Candidate.
Untestable: No binary test exercises explicit merge-candidate-id; implicit latest-candidate path is tested

### B26

IF `fluent attempt run <work-item-id>` is invoked without an
attempt-id and no Attempts exist on the Work Item,
THEN THE SYSTEM SHALL exit non-zero with an error message explaining
no Attempt exists to run.
Test: tests/binary.rs (work_attempt_run_no_attempts_reports_error)

### B27

IF `fluent merge-candidate land <work-item-id>` is invoked without a
merge-candidate-id and no Merge Candidates exist on the Work Item,
THEN THE SYSTEM SHALL exit non-zero with an error message explaining
no Merge Candidate exists to merge.
Test: tests/binary.rs (work_merge_no_candidates_reports_error)

### B28

WHEN `fluent attempt create <work-item-id> <attempt-id>` is invoked for a
stored Work Item,
THE SYSTEM SHALL append a planned Attempt under that Work Item and create
one initial scheduler-facing `write` Task for the Attempt.
Test: tests/binary.rs (work_attempt_adds_planned_attempt_with_initial_write_task)
Test: tests/behaviors/operations/test-work-attempt-intake-review.sh (attempt adds planned Attempt)
Test: tests/behaviors/operations/test-work-attempt-intake-review.sh (attempt adds one initial write Task)

### B29

WHEN `fluent attempt create <work-item-id> <attempt-id>` creates the
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

### B30

WHEN `fluent attempt create <work-item-id> <attempt-id>` creates the
initial `write` Task for a Work Item with instructions,
THE SYSTEM SHALL copy those instructions to the Task so Task execution
can build the coder prompt from durable Work model state.
Test: tests/binary.rs (work_create_persists_instructions_and_attempt_copies_them_to_write_task)
Test: tests/behaviors/operations/test-work-task-instructions.sh (attempt copies instructions to initial write Task)

### B31

WHEN `fluent attempt create <work-item-id> <attempt-id>` creates the
initial `write` Task for a Work Item with planning context and no
explicit instructions,
THE SYSTEM SHALL derive Task instructions from the Work Item planning
context so Task execution can build the coder prompt from durable Work
model state.
Test: tests/binary.rs (work_create_persists_planning_context_and_attempt_copies_it_to_write_task)

### B32

WHEN `fluent task run <work-item-id> <attempt-id> <task-id>` is
invoked for an existing planned `write` Task with exactly one writable
workspace,
THE SYSTEM SHALL create or reuse a registered git worktree at that
workspace path and launch the selected coder in that workspace.
Test: tests/binary.rs (work_task_run_completes_write_task_with_committed_output)
Test: tests/behaviors/operations/test-work-task-run.sh (run reuses worktree and launches coder there)

### B32a

IF the first Writer binding starts while its source checkout contains a
staged, unstaged, or untracked change outside `.fluent/`,
THEN THE SYSTEM SHALL reject the Writer before reserving the Task,
creating or binding its branch or worktree, persisting its baseline, or
launching its coder.
Test: tests/binary.rs (first_writer_rejects_uncommitted_init_instructions_before_side_effects)

### B32b

WHEN the first Writer rejects a source checkout that contains a staged,
unstaged, or untracked change outside `.fluent/`,
THE SYSTEM SHALL name the dirty paths and explain that candidate worktrees
use committed Git state.
Test: tests/binary.rs (first_writer_rejects_uncommitted_init_instructions_before_side_effects)

### B32c

WHEN the first Writer rejects a source checkout that contains a staged,
unstaged, or untracked change outside `.fluent/`,
THE SYSTEM SHALL leave the Attempt and Writer Task planned so the same
command can be retried after the user commits or reverts the changes.
Test: tests/binary.rs (first_writer_rejects_uncommitted_init_instructions_before_side_effects)

### B32d

WHEN the first Writer binding starts while its source checkout contains
changes only under `.fluent/`,
THE SYSTEM SHALL allow candidate-worktree setup to proceed.
Test: tests/binary.rs (first_writer_allows_fluent_only_source_changes)

### B32e

WHEN the user commits the instruction changes reported by `fluent init`
and retries the same planned first Writer,
THE SYSTEM SHALL bind the candidate from that committed source.
Test: tests/binary.rs (committed_init_instructions_reach_first_candidate_and_land)

### B32f

WHEN the first Writer binds a candidate from source that contains committed
Fluent instructions,
THE SYSTEM SHALL include the managed instruction block in the candidate.
Test: tests/binary.rs (committed_init_instructions_reach_first_candidate_and_land)

### B32g

WHEN the first Writer binds a candidate from committed source,
THE SYSTEM SHALL record that source commit as the candidate baseline.
Test: tests/binary.rs (committed_init_instructions_reach_first_candidate_and_land)

### B32h

WHEN a first candidate contains the committed managed instruction block
and completes its required checks,
THE SYSTEM SHALL allow it to complete the public landing route.
Test: tests/binary.rs (committed_init_instructions_reach_first_candidate_and_land)

### B33

WHEN Fluent runs a Work-model behavior review Task for an Attempt whose
Work Item includes a behavior increment,
THE SYSTEM SHALL include the behavior increment explicitly in the review
Task prompt.
Test: tests/binary.rs (work_behavior_review_task_prompt_includes_behavior_increment)

### B34

WHEN Fluent runs a Work-model behavior review Task for an Attempt whose
Work Item does not include a behavior increment,
THE SYSTEM SHALL state in the review Task prompt that no Work behavior
increment was provided.
Test: tests/binary.rs (work_behavior_review_task_prompt_states_missing_behavior_increment)

### B35

WHEN `fluent task run <work-item-id> <attempt-id> <task-id>` or
`fluent attempt run <work-item-id> <attempt-id>` launches a
`write` Task with stored Task instructions,
THE SYSTEM SHALL include those instructions in the coder prompt.
Test: tests/binary.rs (work_task_run_includes_task_instructions_in_coder_prompt)
Test: tests/binary.rs (work_task_run_includes_planning_context_in_coder_prompt)
Test: tests/behaviors/operations/test-work-task-run.sh (run passes Task instructions to coder prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (task run uses durable instructions and keeps extra args out of prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (attempt run uses durable instructions and keeps extra args out of prompt)

### B36

WHEN `fluent task run <work-item-id> <attempt-id> <task-id>` or
`fluent attempt run <work-item-id> <attempt-id>` launches a
`write` Task,
THE SYSTEM SHALL tell the coder that the Task completes only after all
Task output is committed and the writable workspace is clean.
Test: tests/binary.rs (work_task_run_passes_task_context_to_coder_prompt)
Test: tests/behaviors/operations/test-work-task-run.sh (run passes Task context to coder prompt)

### B37

WHEN `fluent task run <work-item-id> <attempt-id> <task-id>` or
`fluent attempt run <work-item-id> <attempt-id>` launches a
`write` Task,
THE SYSTEM SHALL tell the author to perform an upfront scope preflight
that identifies likely touched behavior statements, user-facing docs,
tests, skills/expertise, and verification commands before editing; to
update applicable related artifacts when changing a user-facing command,
behavior, skill, or documentation surface; and to record why related
artifacts do not apply when the Task is intentionally code-only or
docs-only.
Test: tests/binary.rs (work_task_run_passes_task_context_to_coder_prompt)

### B38

WHEN Fluent launches a Work-model follow-up `write` Task that includes
input artifacts,
THE SYSTEM SHALL tell the author to read the review input artifacts
first, address the concrete findings, and check whether each finding
reveals a missing first-pass preflight item.
Test: tests/binary.rs (work_attempt_run_exposes_followup_input_artifacts)

### B39

WHEN Fluent launches a Work-model `review` Task that includes input
artifacts,
THE SYSTEM SHALL name the input artifact paths in the review prompt, grant
read-only access to each exact named file, and tell the reviewer to open those
paths directly before evaluating the candidate, without granting their parent
or sibling artifact directories.
Test: tests/binary.rs (work_task_run_completes_review_task_with_fail_verdict_artifact)
Test: src/work_task_executor.rs (reviewer_readable_inputs_include_every_advertised_exact_file)
Test: tests/binary.rs (reviewer_sandbox_does_not_include_writer_artifact_dir)

### B39a

WHEN a declared Writer or Reviewer exact-file input resolves through a symlink
to a regular file inside the project root, THE SYSTEM SHALL let the Task read
the file through its declared path without granting a parent directory or an
unnamed sibling.
Test: src/work_task_executor.rs (reviewer_project_instruction_grant_follows_an_in_project_file_symlink)
Test: src/work_task_executor.rs (reviewer_profile_reads_an_in_project_instruction_symlink_but_not_its_sibling)

### B39b

IF a declared Writer or Reviewer exact-file input is missing, resolves through
a symlink cycle, resolves to a directory, or resolves outside the project root,
THEN THE SYSTEM SHALL reject it before Task reservation.
Test: src/work_task_executor.rs (declared_file_grant_rejects_a_symlink_outside_its_workspace)
Test: src/work_task_executor.rs (declared_file_grant_rejects_missing_directory_and_cycle_targets)

### B40

WHEN Fluent launches a Work-model `review` Task,
THE SYSTEM SHALL name the Work review artifact path, the exact
filesystem `review.md` path the reviewer must write, and the reviewer
artifact directory; SHALL identify Tester results as host-owned evidence
for the exact candidate commit; SHALL prohibit full-suite reruns; and SHALL
limit the tests reviewer to one named missing evidence check using the
candidate-keyed shared cache rather than the artifact directory.
Test: src/work_task_executor.rs (work_review_prompt_names_work_artifacts_and_writable_outputs)
Test: src/work_task_executor.rs (work_review_prompt_renders_role_conditional_blocks)

### B41

WHEN Fluent plans to launch an Attempt-time tests review Task and the
candidate workspace contains a recognized toolchain marker file
(`Cargo.toml`, `package.json`, `pom.xml`, or `build.gradle`),
THE SYSTEM SHALL copy that toolchain's canonical build directories from
the candidate workspace into one cache keyed by the exact candidate commit
before launching the reviewer and SHALL create no private build directory in
any review artifact area.
Test: src/prep.rs (copies_existing_dirs_and_skips_missing)
Test: src/prep.rs (copies_multiple_node_dirs)
Test: src/work_task_executor.rs (parallel_reviewers_share_one_cache_and_create_no_private_targets)

### B42

WHEN Fluent performs the warm-cache copy,
THE SYSTEM SHALL try a reflink copy first (`cp -c` on macOS,
`cp --reflink=auto` on Linux), fall back to a hardlink copy (`cp -l`)
if reflinks are unsupported, and fall back to a deep copy as a last
resort.
Test: src/prep.rs (copies_existing_dirs_and_skips_missing)

### B43

WHEN Fluent copies a build directory that does not exist in the
candidate workspace,
THE SYSTEM SHALL skip that directory without error.
Test: src/prep.rs (no_error_when_all_dirs_missing)
Test: src/prep.rs (copies_existing_dirs_and_skips_missing)

### B44

WHEN `.fluent/hooks/prepare-pre-review` exists and is executable in
the candidate workspace,
THE SYSTEM SHALL run that hook instead of the built-in auto-prep, with
`FLUENT_REVIEWER_ARTIFACT_DIR` set in the env and CWD = candidate
workspace; SHALL move any canonical cache output into the candidate-keyed
shared cache after admission; and SHALL leave non-cache hook evidence in the
artifact area.
Test: src/hooks.rs (passes_reviewer_artifact_dir_via_env)
Test: src/work_task_executor.rs (custom_hook_moves_cache_out_of_reviewer_artifact)

### B45

WHEN the candidate workspace contains neither a recognized toolchain
marker nor a `prepare-pre-review` hook,
THE SYSTEM SHALL launch the reviewer without any pre-population.
Test: src/prep.rs (returns_none_when_no_marker)

### B46

WHEN Fluent launches a Work-model `review` Task for a candidate
workspace,
THE SYSTEM SHALL include a `git -C <candidate-workspace> diff <range>`
review diff command that shell-quotes the resolved candidate workspace
path and exact revision range so the command can execute through
`/bin/sh`.
Test: src/work_task_executor.rs (work_review_prompt_includes_shell_safe_executable_diff_command)
Test: src/review_diff_command.rs (review_diff_command_survives_apostrophes_through_sh)

### B47

WHEN a behavior operation script invokes the Fluent binary,
THE SYSTEM SHALL allow callers to set `FLUENT_BIN_OVERRIDE` to an
explicit binary path; when no override is set, the script SHALL keep the
repository-local `target/debug/fluent` default.
Test: tests/behaviors/operations/test-behavior-bin-override.sh (operation scripts use FLUENT_BIN_OVERRIDE for debug binary bindings)

### B48

IF a caller passes extra args to `fluent task run` or
`fluent attempt run`,
THE SYSTEM SHALL pass those args only as coder options and SHALL NOT
treat them as additional task prompt content.
Test: tests/binary.rs (work_task_run_keeps_extra_args_out_of_task_prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (task run uses durable instructions and keeps extra args out of prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (attempt run uses durable instructions and keeps extra args out of prompt)

### B49

WHEN a Work Item has no explicit instructions,
THE SYSTEM SHALL preserve the minimal write Task prompt and SHALL NOT
include a `Task instructions:` section.
Test: tests/binary.rs (work_task_run_passes_task_context_to_coder_prompt)
Test: tests/behaviors/operations/test-work-task-instructions.sh (minimal Work Item keeps minimal prompt)

### B50

WHEN a write Task coder exits successfully,
THE SYSTEM SHALL complete the Task only if the writable workspace is
clean and either contains at least one commit produced after Fluent created or
bound the workspace for that Task run, or the Task is a follow-up Writer whose
HEAD still equals the previous completed Writer commit and whose fresh
`no-change.json` records schema version 1, a non-empty reason, and at least one
non-empty command with result `pass`. If the capture Learner later creates a
canonical expertise commit, Fluent SHALL keep the verified Writer commit in
`TaskOutput.base_commit`, move `TaskOutput.commit` and the Merge Candidate to
the canonical commit, and record the exact transition between those identities.
Test: tests/binary.rs (work_task_run_completes_write_task_with_committed_output)
Test: tests/binary.rs (work_task_run_rejects_reused_workspace_without_new_commit)
Test: tests/binary.rs (followup_writer_completes_with_valid_no_change_declaration)
Test: tests/binary.rs (attempt_reviews_unchanged_candidate_after_no_change_writer)
Test: tests/behaviors/operations/test-work-task-run.sh (clean committed Task completes)
Test: tests/behaviors/operations/test-work-task-run.sh (verified follow-up no-change continues into review)
Test: tests/behaviors/operations/test-work-task-run.sh (success without new commit fails with guidance)
Test: src/work_attempt_loop.rs (learner_expertise_commit_preserves_no_change_writer_identity)

### B51

IF the write Task coder exits successfully but the writable workspace has
uncommitted changes,
THEN THE SYSTEM SHALL leave the Task incomplete and report that the Task
must commit or remove the dirty changes.
Test: tests/binary.rs (work_task_run_rejects_dirty_successful_workspace)
Test: tests/binary.rs (followup_writer_rejects_no_change_for_dirty_or_divergent_candidate)
Test: tests/behaviors/operations/test-work-task-run.sh (dirty successful Task fails with guidance)

### B52

IF the write Task coder exits successfully but the writable workspace has
no committed Task output produced by that run and does not qualify as a verified
follow-up no-change output,
THEN THE SYSTEM SHALL leave the Task incomplete and report that there is
no committed Task output or name the valid declaration required. The Task and
Attempt SHALL pause at a resumable Writer-completion boundary when the clean
workspace has no committed output and completion fails on missing or invalid
no-change evidence, so one explicit resume retries that same Task. Initial
Writers, malformed or empty declarations, stale retry declarations, dirty
workspaces, and HEADs that differ from the previous completed Writer commit do
not qualify. A persisted no-change output whose verified and current commits
differ also does not qualify unless a capture Learner record names that exact
`from_commit` and `to_commit` transition; missing, partial, reversed, and
mismatched transitions SHALL fail Work-model validation.
Test: tests/binary.rs (work_task_run_rejects_success_without_commits)
Test: tests/binary.rs (work_task_run_rejects_reused_workspace_without_new_commit)
Test: tests/binary.rs (initial_writer_rejects_no_change_declaration_without_commit)
Test: tests/binary.rs (followup_writer_rejects_missing_or_invalid_no_change_declaration)
Test: tests/binary.rs (followup_writer_rejects_no_change_for_dirty_or_divergent_candidate)
Test: tests/binary.rs (followup_writer_clears_stale_no_change_declaration_before_retry)
Test: tests/binary.rs (corrective_writer_commit_ignores_no_change_declaration)
Test: tests/behaviors/operations/test-work-task-run.sh (success without new commit fails with guidance)
Test: src/work_model.rs (no_change_commit_divergence_requires_successful_capture_learning)
Test: src/work_task_executor.rs (invalid_writer_completion_evidence_pauses_the_same_task)

### B53

WHEN Fluent creates a `write` Task (initial author Task or follow-up
after failed review),
THE SYSTEM SHALL set `Task.artifact_area` to
`Some(TaskArtifactArea { path:
".fluent/work/artifacts/<work-item-id>/<attempt-id>/<task-id>" })`
following the same convention as review and tester Tasks.
Test: src/work_model.rs (initial_write_task_has_artifact_area_path)
Test: src/work_model.rs (followup_write_task_has_artifact_area_path)
Test: tests/work_model_external.rs (work_item_add_initial_attempt_creates_scheduler_facing_write_task)

### B54

WHEN a `write` Task is executed,
THE SYSTEM SHALL pass the path `<artifact_area>/transcript.jsonl` to
the Coder's transcript argument so the agent's session is persisted
to that file during execution.
Test: tests/binary.rs (write_task_transcript_persists_after_successful_attempt)

### B55

WHEN the writer's sandbox is constructed for a `write` Task,
THE SYSTEM SHALL grant write access to both the candidate workspace
(existing behavior) AND the Task's artifact directory.
Test: tests/binary.rs (write_task_sandbox_grants_artifact_dir_write_access)

### B56

WHEN a `write` Task completes,
THE SYSTEM SHALL leave the transcript file in place under the artifact
directory; the file SHALL NOT be deleted when the Task transitions to
`complete`, `failed`, or `needs-user`.
Test: tests/binary.rs (write_task_transcript_persists_after_successful_attempt)

### B57

WHEN `fluent work-item show <work-item-id>` returns Task JSON for a
completed `write` Task,
THE SYSTEM SHALL include the `artifact_area.path` field populated with
the expected artifact directory path.
Test: tests/binary.rs (write_task_transcript_persists_after_successful_attempt)

### B58

WHEN a `write` Task fails before producing a commit,
THE SYSTEM SHALL still persist whatever transcript content the Coder
wrote, so post-mortem analysis is possible.
Test: tests/binary.rs (write_task_transcript_persists_after_failed_attempt)

### B59

WHEN any reviewer Task (architecture, behaviors, documentation, skills,
tests) is executed,
THE SYSTEM SHALL receive read-only access to the exact Writer transcript and
completion-matrix files named in its prompt while the Writer artifact directory
and every unnamed sibling remain inaccessible.
Test: tests/binary.rs (reviewer_sandbox_does_not_include_writer_artifact_dir)

### B60

WHEN a `review` Task is executed,
THE SYSTEM SHALL pass the path `<artifact_area>/transcript.jsonl` to
the Coder's transcript argument so the agent's session is persisted
to that file during execution.
Test: src/work_task_executor.rs (review_task_transcript_path_resolved_from_artifact_area)
Test: tests/binary.rs (review_task_transcript_persists_after_completion)

### B61

WHEN a `review` Task completes (verdict pass, fail, or uncertain),
THE SYSTEM SHALL leave the `transcript.jsonl` file in place under the
artifact directory alongside `review.md`. The transcript SHALL NOT be
deleted when the Task transitions to `complete`, `failed`, or
`needs-user`.
Test: tests/binary.rs (review_task_transcript_persists_after_completion)

### B62

WHEN a `review` Task fails before producing `review.md`,
THE SYSTEM SHALL still persist whatever transcript content the Coder
wrote, so post-mortem analysis is possible.
Test: tests/binary.rs (review_task_transcript_persists_on_failure)

### B63

WHEN any reviewer Task (architecture, behaviors-completeness,
documentation, skills, tests-completeness) is executed,
THE SYSTEM SHALL NOT receive read access to other reviewers'
transcripts in its sandbox readable roots, and the reviewer prompt
SHALL NOT mention any other reviewer's transcript file.
Test: tests/binary.rs (reviewer_sandbox_does_not_include_other_reviewer_artifact_dirs)

### B64

IF an existing candidate workspace or named input path is unreadable under a
Review Task's rendered sandbox, THEN THE SYSTEM SHALL pause the Attempt for
user recovery without reserving the Task or launching the reviewer.
Test: src/os.rs (readable_path_preflight_accepts_exact_file_and_rejects_sibling)
Test: src/work_task_executor.rs (reviewer_read_path_preflight_failure_precedes_reservation)

### B65

WHEN scheduler usage logging runs after a `write` or `review` Task,
THE SYSTEM SHALL find the transcript at the documented path and append
per-turn token usage rows to `~/.config/fluent/usage/usage.jsonl`
with the documented schema, identifying the Task via `work_item_id`,
`attempt_id`, and `task_id`.
Test: src/usage.rs (log_usage_from_transcript appends rows when
transcript file exists; write and review Tasks provide transcript
paths, so the existing usage-logging tests cover the parsing path)

### B66

WHEN a `rebase` Task is created,
THE SYSTEM SHALL continue to use the rebase Task's existing artifact
directory (no change to the rebase Task surface).
Untestable: structural constraint; rebase Tasks are a separate kind
with their own creation path not affected by this change.

### B67

WHEN `fluent review <work-item-id> <attempt-id>` is invoked for an
Attempt with completed write output,
THE SYSTEM SHALL append planned `review` Tasks that read the candidate
workspace, declare no writable workspace, and declare artifact areas
under `.fluent/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`.
Test: tests/binary.rs (work_review_plans_review_tasks_for_completed_attempt)
Test: tests/behaviors/operations/test-work-task-run.sh (review planning adds read-only Task without changing candidate)

### B68

IF `fluent review <work-item-id> <attempt-id>` is invoked for an
Attempt without completed write output,
THEN THE SYSTEM SHALL exit non-zero and leave stored Work Item state
unchanged.
Test: tests/binary.rs (work_review_requires_completed_write_output)
Test: tests/behaviors/operations/test-work-task-run.sh (review planning requires completed write output)

### B69

WHEN `fluent review codebase <work-item-id> <attempt-id>` is
invoked (without `--from-working-tree`) for a stored Work Item with no
existing Attempt of that id,
THE SYSTEM SHALL append a review-only Attempt with a planned Tester Task
scheduled before the planned review Tasks for the default reviewer set,
and SHALL set the workspace for every Task in the Attempt to the
review-only worktree for the current branch
(`../work-review-<sanitized-branch>/`).
Test: tests/binary.rs (work_review_codebase_default_creates_worktree_attempt_with_tester)

### B70

WHEN `fluent review codebase <work-item-id> <attempt-id>
--from-working-tree` is invoked for a stored Work Item with no existing
Attempt of that id,
THE SYSTEM SHALL append a review-only Attempt with planned review Tasks
only (no Tester Task) for the default reviewer set, and SHALL set the
workspace for every review Task to the current source checkout (`.`)
under the restorative source-checkout guard.
Test: tests/binary.rs (work_review_codebase_creates_review_only_attempt)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-codebase creates review-only Attempt)

### B71

WHEN a review-only Attempt is created against the source checkout
(`--from-working-tree`),
THE SYSTEM SHALL give each review Task read-only access to the current
source checkout and a managed artifact area under
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`.
Test: tests/binary.rs (work_review_codebase_creates_review_only_attempt)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-codebase creates review-only Attempt)

### B72

IF `fluent review codebase <work-item-id> <attempt-id>` is invoked
for a missing Work Item or duplicate Attempt id,
THEN THE SYSTEM SHALL exit non-zero without changing Work state.
Test: tests/binary.rs (work_review_codebase_missing_or_duplicate_leaves_state_unchanged)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-codebase rejects missing and duplicate)

### B73

WHEN `fluent task run <work-item-id> <attempt-id> <task-id>` is
invoked for a planned `review` Task,
THE SYSTEM SHALL complete the Task after the reviewer writes `review.md`
under the Task artifact area, even when that artifact contains
`Verdict: fail` or `Verdict: uncertain`.
Test: tests/binary.rs (work_task_run_completes_review_task_with_fail_verdict_artifact)
Test: tests/behaviors/operations/test-work-task-run.sh (review Task with fail verdict completes)
Test: tests/behaviors/operations/test-work-task-run.sh (review Task with uncertain verdict completes)

### B74

WHEN `fluent attempt run <work-item-id> <attempt-id>` is invoked
for an Attempt with a planned write Task,
THE SYSTEM SHALL run the write Task through the existing Task executor,
then reload stored Work Item state before planning later transitions.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

### B75

WHEN `fluent attempt run <work-item-id> <attempt-id>` advances a
normal Work Attempt whose initial write Task has completed and no review
round is planned for that write output,
THE SYSTEM SHALL plan initial review Tasks using the full Work reviewer
set and run planned review Tasks through the existing Task executor.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

### B76

WHEN a normal Work Attempt completes a write round created from
failed review artifacts,
THE SYSTEM SHALL plan the next review round only for the failed reviewer
roles that fed that write round.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_mixed_failed_and_uncertain_reviews)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop plans follow-up with mixed missing review)

### B77

WHEN a normal Work Attempt completes a write round created from
failed review artifacts and Fluent plans a targeted follow-up review
round,
THE SYSTEM SHALL attach the relevant prior failed review artifact for
each planned review Task role as that review Task's input artifact.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_mixed_failed_and_uncertain_reviews)

### B78

IF a normal Work Attempt completes a write round and the failed
reviewer roles cannot be derived from the follow-up Task input artifacts,
THEN THE SYSTEM SHALL plan the next review round using the full Work
reviewer set.
Test: src/work_attempt_loop.rs (unmappable_followup_inputs_fall_back_to_full_review_roles)

### B79

WHEN `fluent attempt run <work-item-id> <attempt-id>` is invoked
for a normal Work Attempt with planned review Tasks,
THE SYSTEM SHALL run the planned review Tasks in parallel with
concurrency limited to `FLUENT_MAX_PARALLEL_REVIEWERS` (default 5,
minimum 1) before planning later transitions.
Test: src/work_attempt_loop.rs (cap_enforcement_limits_in_flight_reviewers)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

### B80

WHEN `fluent attempt run <work-item-id> <attempt-id>` is invoked
for a review-only Attempt whose workspace is a review-only worktree
(`../work-review-<sanitized-branch>/`) with planned review Tasks,
THE SYSTEM SHALL run the planned review Tasks in parallel with
concurrency limited to `FLUENT_MAX_PARALLEL_REVIEWERS` (default 5,
minimum 1).
Test: tests/binary.rs (post_merge_review_creates_worktree_and_runs_tester_then_reviewers)

### B81

WHEN `fluent attempt run <work-item-id> <attempt-id>` is invoked
for a review-only Attempt whose workspace is the source checkout (`.`)
with planned review Tasks,
THE SYSTEM SHALL run the planned review Tasks serially because reviewers
share the source checkout under the restorative guard.
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop runs planned review Tasks)

### B82

WHEN the planned review-task count for a round exceeds the configured
`FLUENT_MAX_PARALLEL_REVIEWERS` cap,
THE SYSTEM SHALL queue excess tasks and launch each as in-flight slots
free up, keeping at most `cap` reviewer threads in flight at any time.
Test: src/work_attempt_loop.rs (cap_enforcement_limits_in_flight_reviewers)

### B82a

WHEN Writer, reviewer, and Learner logical runs use the same provider across
concurrent Fluent processes or projects,
THE SYSTEM SHALL admit at most the user-wide provider concurrency limit.
Test: src/provider_admission.rs (shared_ceiling_bounds_total_in_flight_runs)
Test: src/provider_admission.rs (cross_process_ceiling_and_exit_release_slot)

### B82b

WHEN concurrent logical runs use different providers,
THE SYSTEM SHALL admit them through independent provider capacity pools.
Test: src/provider_admission.rs (providers_use_independent_pools)

### B82c

WHEN a Writer or reviewer waits for provider capacity before Task reservation,
THE SYSTEM SHALL leave the Task planned, stop waiting if the Task becomes
ineligible, and consume no Task execution state.
Test: src/work_task_executor.rs (capacity_wait_leaves_task_planned_and_observes_cancellation)

### B82d

WHEN an admitted logical provider run ends or is cancelled,
THE SYSTEM SHALL release its provider slot automatically.
Test: src/provider_admission.rs (cancelled_run_drop_releases_shared_slot)
Test: src/provider_admission.rs (cross_process_ceiling_and_exit_release_slot)

### B82e

IF the coder-owned rate-limit retry budget exhausts,
THEN THE SYSTEM SHALL return a typed terminal error and SHALL NOT spend the
generic Task retry budget on another copy of the same logical run.
Test: src/coder.rs (rate_limit_retry_helper_exhausts_every_provider_launch)
Test: src/work_task_executor.rs (terminal_coder_failures_bypass_generic_retries)

### B83

WHEN `fluent merge-candidate land <work-item-id> <merge-candidate-id>` finishes
the fast-forward,
THE SYSTEM SHALL invoke the landed Learner handoff recovery boundary and
durably record its complete or incomplete result before evaluating optional
post-merge review.
Test: src/work_merge_executor.rs (fresh_land_records_follow_up_outcome_before_post_merge_review)

### B83a

WHEN `fluent merge-candidate land` is invoked without
`--post-merge-review`,
THE SYSTEM SHALL append no post-merge-review queue entry.
Test: tests/binary.rs (work_merge_candidate_land_default_has_no_post_merge_review_side_effects)

### B83a1

WHEN `fluent merge-candidate land` is invoked without
`--post-merge-review`,
THE SYSTEM SHALL spawn no post-merge-review runner process.
Test: tests/binary.rs (work_merge_candidate_land_default_has_no_post_merge_review_side_effects)

### B83a1a

WHEN `fluent merge-candidate land` is invoked without
`--post-merge-review`,
THE SYSTEM SHALL write no post-merge-review runner log.
Test: tests/binary.rs (work_merge_candidate_land_default_has_no_post_merge_review_side_effects)

### B83a2

WHEN `fluent merge-candidate land` is invoked without
`--post-merge-review`,
THE SYSTEM SHALL create no post-merge-review Attempt.
Test: tests/binary.rs (work_merge_candidate_land_default_has_no_post_merge_review_side_effects)

### B83a3

WHEN `fluent merge-candidate land` is invoked without
`--post-merge-review`,
THE SYSTEM SHALL create no post-merge-review worktree.
Test: tests/binary.rs (work_merge_candidate_land_default_has_no_post_merge_review_side_effects)

### B83a4

WHEN `fluent merge-candidate land` is invoked without
`--post-merge-review`,
THE SYSTEM SHALL run no post-merge-review LLM inside the land command.
Test: tests/binary.rs (work_merge_candidate_land_default_has_no_post_merge_review_side_effects)

### B83b

WHERE `--post-merge-review` was passed and the landed Learner handoff boundary
durably returned either complete or incomplete,
THE SYSTEM SHALL invoke post-merge-review scheduling only after that durable
result is observable from the Work model store.
Test: src/work_merge_executor.rs (fresh_land_records_follow_up_outcome_before_post_merge_review)
Test: tests/binary.rs (explicit_post_merge_review_records_handoff_failure_before_scheduling)

### B83b1

WHERE an affirmative post-merge-review request reaches its scheduling
boundary,
THE SYSTEM SHALL append one queue entry containing the target branch, merged
commit, timestamp, source Work Item, source Merge Candidate, and pre-land base
commit.
Test: src/work_merge_executor.rs (recovered_fresh_land_preserves_base_commit_for_post_merge_review)
Test: tests/binary.rs (explicit_post_merge_review_records_handoff_failure_before_scheduling)

### B83b2

WHERE an affirmative post-merge-review request appends its queue entry,
THE SYSTEM SHALL spawn a detached post-merge-review runner.
Test: tests/binary.rs (work_merge_candidate_land_explicit_post_merge_review_enqueues_and_spawns)

### B83c

IF the landed Learner handoff boundary cannot durably persist either a complete
or incomplete result,
THEN THE SYSTEM SHALL schedule no post-merge review.
Test: src/work_merge_executor.rs (fresh_land_skips_post_merge_review_when_follow_up_persistence_is_unknown)

### B83c1

IF the landed Learner handoff boundary cannot durably persist either a complete
or incomplete result after the merge is durable,
THEN THE SYSTEM SHALL return the successful landed outcome unchanged.
Test: src/work_merge_executor.rs (fresh_land_skips_post_merge_review_when_follow_up_persistence_is_unknown)

### B83d

WHERE `--no-post-merge-review` is passed to `fluent merge-candidate land`,
THE SYSTEM SHALL accept it as the compatibility spelling for omission.
Test: tests/binary.rs (work_merge_candidate_land_no_post_merge_review_remains_compatible)

### B83d0

WHERE `--no-post-merge-review` is passed to `fluent merge-candidate land`,
THE SYSTEM SHALL append no post-merge-review queue entry.
Test: tests/binary.rs (work_merge_candidate_land_no_post_merge_review_remains_compatible)

### B83d1

WHERE `--no-post-merge-review` is passed to `fluent merge-candidate land`,
THE SYSTEM SHALL still process the landed Learner handoff.
Test: tests/binary.rs (no_post_merge_review_does_not_skip_learner_handoff)

### B83e

IF both `--post-merge-review` and `--no-post-merge-review` are passed,
THEN THE SYSTEM SHALL reject the command before changing the target ref, Merge
Candidate, Learner handoff, or either queue.
Test: tests/binary.rs (merge_candidate_land_rejects_conflicting_post_merge_review_options_without_mutation)

### B83f

WHEN an idempotent re-land resumes landed Learner handoff recovery,
THE SYSTEM SHALL NOT schedule post-merge review, including when the positive
option is passed on the re-land.
Test: tests/binary.rs (reland_does_not_retroactively_schedule_post_merge_review)

### B83g

WHEN `fluent merge-candidate land` runs with `--runtime fargate` without
`--post-merge-review`,
THE SYSTEM SHALL carry no post-merge-review environment override.
Test: src/fargate.rs (work_merge_overrides_omit_post_merge_review_by_default)

### B83g1

WHEN the Fargate entrypoint receives no post-merge-review environment override,
THE SYSTEM SHALL invoke the inner land without a post-merge-review option.
Test: tests/behaviors/operations/test-fargate-entrypoint-post-merge-review.sh (absent request carries no post-merge-review flag)

### B83h

WHEN `fluent merge-candidate land` runs with `--runtime fargate` and
`--post-merge-review`,
THE SYSTEM SHALL propagate the affirmative request to the Fargate container via
the `FLUENT_POST_MERGE_REVIEW=1` environment variable.
Test: src/fargate.rs (work_merge_overrides_include_positive_post_merge_review)

### B83i

WHEN the Fargate entrypoint receives `FLUENT_POST_MERGE_REVIEW=1`,
THE SYSTEM SHALL pass `--post-merge-review` to the inner
`fluent merge-candidate land` invocation.
Test: tests/behaviors/operations/test-fargate-entrypoint-post-merge-review.sh (positive request reaches inner land)

### B84

WHEN `fluent post-merge-review run` runs,
THE SYSTEM SHALL sleep the debounce window, then for each target
branch with a queued entry at least `debounce_seconds` old, run a
review-only Attempt inside the review-only worktree for that branch
at the queued merged commit, using a Tester Task scheduled before the
full reviewer set, and clear processed queue entries.
Test: tests/binary.rs (post_merge_review_creates_worktree_and_runs_tester_then_reviewers)

### B85

WHEN multiple merges arrive for the same target branch within the
debounce window,
THE SYSTEM SHALL coalesce them — only the latest entry triggers a
review; earlier detached children wake up, see a newer entry, and
exit. The single review covers the cumulative range.
Untestable: Requires concurrent detached processes racing on debounce window timing

### B86

WHEN the post-merge review finds any reviewer artifact with a failing
or uncertain verdict,
THE SYSTEM SHALL create a post-merge-review-fix Work Item with the
failed review artifacts as planning context and run its first Attempt.
A passing Merge Candidate is left pending — it lands when a live
`auto-merge` watcher picks it up or the user invokes
`fluent merge-candidate land`, exactly like any other candidate.
The fix-depth recursion bound is persisted on Work Item state
and checked at land time; see the
"Post-merge forward-fix landing" area for details.
Untestable: Requires end-to-end post-merge review with LLM reviewers and auto-fix

### B87

WHEN the post-merge review runner creates a review-only Attempt for a
queued entry,
THE SYSTEM SHALL set every Task's workspace to the review-only worktree
for the target branch (`../work-review-<sanitized-branch>/`) and schedule
a Tester Task before the review Tasks; the work-attempt loop ensures the
worktree exists at the queued merged commit before the Tester runs.
Test: tests/binary.rs (post_merge_review_creates_worktree_and_runs_tester_then_reviewers)
Test: src/work_model.rs (post_merge_review_attempt_round_trips_through_storage)

### B88

WHERE a fresh land has an affirmative post-merge-review request,
THE SYSTEM SHALL record the target branch's head immediately before the merge
as the queue entry's `base_commit`.
Test: src/work_merge_executor.rs (recovered_fresh_land_preserves_base_commit_for_post_merge_review)
Test: tests/binary.rs (explicit_post_merge_review_records_handoff_failure_before_scheduling)

### B88a

WHEN the post-merge review runner creates a review-only Attempt from a queue
entry with a `base_commit`,
THE SYSTEM SHALL copy that `base_commit` onto each Task's review context.
Test: src/work_model.rs (post_merge_review_attempt_round_trips_through_storage)

### B88b

WHEN a review Task has a `base_commit`,
THE SYSTEM SHALL include a `git -C <worktree> diff <base_commit>..HEAD` command
in the review prompt.
Test: src/work_task_executor.rs (work_review_prompt_populates_diff_command_for_post_merge_when_base_commit_present)

### B88c

WHEN a review-only Task has no `base_commit`,
THE SYSTEM SHALL omit the review diff command from the prompt.
Test: src/work_task_executor.rs (work_review_prompt_omits_diff_command_for_review_only_without_base_commit)

### B89

WHEN `fluent review codebase` is invoked interactively by the
user against the current source checkout,
THE SYSTEM SHALL apply the existing restorative guard semantics:
snapshot non-Fluent worktree state and protected `.fluent/`
contents at begin, restore both on finish if reviewers modified
them, and surface clear errors when restoration was needed.
Test: tests/binary.rs (work_attempt_run_review_only_rejects_source_changes)
Test: tests/binary.rs (work_attempt_run_review_only_rejects_fluent_state_changes)
Test: tests/binary.rs (work_attempt_run_review_only_restores_mixed_source_and_fluent_changes)

### B90

WHEN a reviewer task modifies a file inside its allowed reviewer
artifact directory,
THE SYSTEM SHALL leave that change in place (artifact directories are
the reviewer's writable surface, unchanged from today).
Test: tests/binary.rs (work_attempt_run_review_only_passes_without_merge_candidate)

### B91

IF the post-merge review's review-only Attempt completes with at
least one reviewer task whose review.md has `Verdict: fail` or
`Verdict: uncertain`,
THEN THE SYSTEM SHALL collect those review artifacts and proceed to
auto-create a `post-merge-review-fix-<branch>-<timestamp>` Work
Item, regardless of whether peer reviewer tasks are in
`failed`/`needs-user`/`complete` status.
Untestable: Requires end-to-end post-merge review Attempt with mixed reviewer task states

### B92

WHEN the post-merge review reads completed review artifacts to
decide whether to create a forward-fix Work Item,
THE SYSTEM SHALL include reviewers whose Task status is `failed`
in addition to `complete`, treating any reviewer that wrote a
review.md with a non-pass verdict as a finding source.
Untestable: Requires end-to-end post-merge review with failed-status reviewer artifacts

### B93

IF the post-merge review cannot create a forward-fix Work Item
(e.g., because storage write fails),
THEN THE SYSTEM SHALL log the failure to the post-merge review log
and leave the synthetic Work Item state intact so an operator can
inspect the findings manually.
Untestable: Requires storage write failure injection during post-merge forward-fix creation

### B94

WHEN the work-attempt loop starts a review-only Attempt whose workspace
is a review-only worktree and that worktree does not yet exist on disk,
THE SYSTEM SHALL create it as a sibling Git worktree at
`../work-review-<sanitized-branch>/` checked out detached at the recorded
candidate commit before the Tester Task runs.
Test: tests/binary.rs (post_merge_review_creates_worktree_and_runs_tester_then_reviewers)

### B95

WHEN the work-attempt loop starts a review-only Attempt whose workspace
is a review-only worktree and that worktree already exists,
THE SYSTEM SHALL sync it to the recorded candidate commit (`git checkout
--detach <commit>` and `git reset --hard <commit>`) before the Tester
Task runs, reusing the existing directory rather than recreating it.
Test: tests/binary.rs (post_merge_review_creates_worktree_and_runs_tester_then_reviewers)

### B96

IF the existing review-only worktree has uncommitted tracked changes
when the work-attempt loop tries to sync it,
THEN THE SYSTEM SHALL reset it (`git reset --hard && git clean -fdx`)
before checking out the recorded candidate commit.
Untestable: Requires the worktree to be left in a dirty state between Attempts; covered by inspection of `src/review_only_worktree.rs::sync`

### B97

WHEN a review-only Attempt completes,
THE SYSTEM SHALL retain its review-only worktree on disk so that the
next review-only Attempt targeting the same branch reuses the warm cache
of build outputs inside it.
Test: tests/binary.rs (post_merge_review_creates_worktree_and_runs_tester_then_reviewers)

### B98

WHEN a review-only Attempt with a planned Tester Task is started,
THE WORK-ATTEMPT LOOP SHALL run the Tester Task to completion, then plan
and run the review Tasks regardless of the Tester Task's outcome (each
reviewer receives the Tester's `tester-results.json` as an input
artifact).
Test: tests/binary.rs (post_merge_review_creates_worktree_and_runs_tester_then_reviewers)
Test: src/work_model.rs (post_merge_review_attempt_round_trips_through_storage)

### B99

WHEN a review Task in a review-only Attempt is about to run,
THE SYSTEM SHALL use existing exact-commit evidence by default, SHALL permit the
tests reviewer at most one named missing evidence check, and SHALL NOT provision
a shared or artifact-local build cache from the source checkout.
Test: src/work_task_executor.rs (work_review_prompt_omits_diff_command_for_review_only_without_base_commit)

### B100

WHEN `fluent attempt run <work-item-id> <attempt-id>` is invoked
for a review-only Attempt whose review-only worktree is already in use
by another in-flight review-only Attempt (state-based detection: any
review-only-like Attempt against the same worktree whose status is
`executing` or `reviewing`),
THE SYSTEM SHALL exit non-zero with an error message naming the
in-flight Work Item and Attempt, and SHALL leave both Attempts' stored
states unchanged.
Test: tests/binary.rs (work_attempt_run_rejects_review_only_worktree_already_in_flight)

### B101

WHILE a review-only Attempt is being processed against the review-only
worktree for a target branch,
THE POST-MERGE REVIEW SUBPROCESS SHALL skip any queued entry whose
target branch maps to that worktree, leaving the entry in the queue for
the next pass.
Test: tests/binary.rs (post_merge_review_defers_queue_entry_when_worktree_in_flight)

### B102

WHEN `fluent cleanup` is invoked without
options,
THE SYSTEM SHALL remove every registered review-only worktree
(`../work-review-*`) whose corresponding local branch no longer exists,
and SHALL leave others in place.
Test: tests/binary.rs (review_only_worktree_prune_default_removes_orphan_keeps_others)

### B103

WHEN `fluent cleanup --prune-all-review-worktrees` is invoked,
THE SYSTEM SHALL remove every registered review-only worktree present,
regardless of whether the corresponding branch still exists.
Test: tests/binary.rs (review_only_worktree_prune_all_removes_everything_but_in_use)

### B104

IF `fluent cleanup` would remove a review-only
worktree currently in use by an in-flight review-only Attempt,
THEN THE SYSTEM SHALL skip that worktree and continue with the others,
printing a notice naming the in-flight Work Item and Attempt.
Test: tests/binary.rs (review_only_worktree_prune_default_removes_orphan_keeps_others)
Test: tests/binary.rs (review_only_worktree_prune_all_removes_everything_but_in_use)

### B105

WHEN `fluent cleanup` (dry run is the default) is invoked
(with or without `--all`),
THE SYSTEM SHALL report the worktrees that would be removed and any
that would be skipped (with reasons) without actually removing any, and
SHALL exit successfully.
Test: tests/binary.rs (review_only_worktree_prune_dry_run_changes_nothing)

### B106

WHEN `fluent cleanup` completes (in any mode),
THE SYSTEM SHALL print a summary line counting removed, skipped-in-use,
and kept worktrees.
Test: tests/binary.rs (review_only_worktree_prune_default_removes_orphan_keeps_others)
Test: tests/binary.rs (review_only_worktree_prune_all_removes_everything_but_in_use)
Test: tests/binary.rs (review_only_worktree_prune_dry_run_changes_nothing)

### B107

WHEN `fluent post-merge-review run` starts a pass,
THE SYSTEM SHALL prune orphan review-only worktrees (the same default
mode the manual `fluent cleanup` command
uses, including the "skip if in use" rule) before processing queued
entries, and log removed and skipped paths to the post-merge review
log.
Test: tests/binary.rs (post_merge_review_auto_prunes_orphan_worktree_before_processing_queue)

### B108

WHEN `fluent cleanup` runs and finds a sibling directory matching
`../review-<bytelen>-<work-item-id>-<attempt-id>-<reviewer>` whose
Work Item has no merge candidate currently executing,
THE SYSTEM SHALL list the directory in the dry-run report; with
`--apply`, THE SYSTEM SHALL remove the directory and any registered
git worktree pointing at it.
Test: src/cleanup.rs (parse_reviewer_worktree_name_extracts_components, parse_reviewer_worktree_name_handles_long_work_item_id, parse_reviewer_worktree_name_rejects_non_reviewer_suffix, parse_reviewer_worktree_name_rejects_non_matching_names, stranded_reviewer_worktree_detected_for_non_executing_work_item, stranded_reviewer_worktree_preserved_for_executing_merge_candidate, stranded_reviewer_worktree_removed_on_apply)

### B109

WHEN all review Tasks for an Attempt review round complete and all
review artifacts have passing verdicts,
THE SYSTEM SHALL mark the Attempt review state as `passed`, leave the
Attempt `complete`, create one durable Merge Candidate, and report the
Merge Candidate id.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

### B110

WHEN `fluent attempt run <work-item-id> <attempt-id>` is invoked
for a review-only Attempt with planned review Tasks,
THE SYSTEM SHALL run those reviewer Tasks, require each reviewer to write
its Work review artifact, and leave non-Fluent source files unchanged.
Test: tests/binary.rs (work_attempt_run_review_only_passes_without_merge_candidate)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only pass completes without Merge Candidate)

### B111

WHEN a review-only review Task starts,
THE SYSTEM SHALL require the source checkout HEAD to match the source
commit recorded in the Task review context.
Test: tests/binary.rs (work_attempt_run_review_only_requires_recorded_source_commit)

### B112

WHEN a review-only review Task changes source files or Fluent-owned
state outside its managed artifact area,
THE SYSTEM SHALL mark the Task failed, restore changed non-Fluent
source files, and report that the source checkout changed outside the
allowed boundary.
Test: tests/binary.rs (work_attempt_run_review_only_rejects_source_changes)
Test: tests/binary.rs (work_attempt_run_review_only_rejects_fluent_state_changes)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only rejects source changes)

### B113

WHEN all review-only reviewer artifacts have verdict `pass`,
THE SYSTEM SHALL mark the Attempt complete with review state `passed`
and SHALL NOT create a Merge Candidate.
Test: tests/binary.rs (work_attempt_run_review_only_passes_without_merge_candidate)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only pass completes without Merge Candidate)

### B114

WHEN any review-only reviewer artifact has verdict `fail`,
THE SYSTEM SHALL mark the Attempt failed with review state `failed` and
SHALL NOT create a write round or Merge Candidate.
Test: tests/binary.rs (work_attempt_run_review_only_fails_without_followup)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only fail stops without follow-up)

### B115

WHEN any review-only reviewer artifact has verdict `uncertain` and none
has verdict `fail`,
THE SYSTEM SHALL mark the Attempt `needs-user`, write a Work handoff
artifact, and SHALL NOT create a write round or Merge
Candidate.
Test: tests/binary.rs (work_attempt_run_review_only_uncertain_needs_user)
Test: tests/behaviors/operations/test-work-review-codebase.sh (review-only uncertain needs user)

### B116

WHEN a Merge Candidate is created from a passed Attempt,
THE SYSTEM SHALL record the source candidate workspace, target workspace,
source branch, and candidate commit from the latest completed write Task,
initialize the target branch from that write Task's source branch, and set
the Merge Candidate review state to pending.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

### B117

IF `fluent attempt run <work-item-id> <attempt-id>` is invoked for
an Attempt whose reviews and Learner already passed, whose Merge Candidate
already exists, and which has no pending landed-follow-up recovery,
THEN THE SYSTEM SHALL leave Work Item state unchanged and report the
existing Merge Candidate.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop invalid or terminal request leaves state unchanged)

### B118

WHEN `fluent merge-candidate show <work-item-id> <merge-candidate-id>`
is invoked,
THE SYSTEM SHALL print the stored Merge Candidate as pretty JSON.
Test: tests/binary.rs (work_attempt_run_drives_write_reviews_and_passes)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop passes review round)

### B119

IF `fluent merge-candidate show <work-item-id> <merge-candidate-id>` is
invoked for a missing Work Item or missing Merge Candidate,
THEN THE SYSTEM SHALL exit non-zero and leave Work Item state unchanged.
Test: tests/binary.rs (work_merge_candidate_missing_item_or_candidate_reports_error)

### B120

WHEN `fluent merge-candidate land <work-item-id> <merge-candidate-id>` is invoked
for a capture-mode Merge Candidate that still needs to land,
THE SYSTEM SHALL resolve the exact current-target baseline, rebase the candidate,
run configured pre-merge checks, verify the exact final candidate with the Tester,
and fast-forward only after those deterministic checks pass.
Test: tests/binary.rs (work_merge_candidate_runs_tester_for_rebased_tip_before_land)

The former B121–B127 merge-time reviewer requirements are retired. Landing does
not launch reviewers; Attempt reviews precede its final Tester and Learning.

### B128

WHEN `fluent merge-candidate land <work-item-id> <merge-candidate-id>` is invoked
for a Merge Candidate with merge status `merged` and a stored
`merged_commit`,
THE SYSTEM SHALL report the stored merged commit without resolving
workspaces, rebasing, running checks, running reviewers, or moving the
target branch.
Test: tests/binary.rs (work_merge_candidate_rerun_after_cleanup_preserves_landed_state)

### B129

IF `fluent merge-candidate land <work-item-id> <merge-candidate-id>` is invoked
for a Merge Candidate whose stored provenance no longer matches the
passed Attempt output,
THEN THE SYSTEM SHALL leave the target branch and stored Merge Candidate
state unchanged.
Test: tests/binary.rs (work_merge_candidate_rejects_stale_stored_provenance_without_rewrite)

### B130

WHEN `fluent merge-candidate land <work-item-id> <merge-candidate-id>` reaches
the rebase step,
THE SYSTEM SHALL invoke an agent to perform `git rebase <target>` inside
the candidate workspace and produce a rebased candidate-tip commit,
regardless of whether conflicts would have arisen from a non-agentic
rebase.
Test: tests/binary.rs (work_merge_candidate_rebases_when_target_advanced)

### B131

WHEN the rebase step is invoked,
THE SYSTEM SHALL record the rebase as a Task on the Attempt with its own
ID, kind `rebase`, artifact directory, prompt log, and status, visible
via `fluent work-item show <work-item-id>`.
Test: tests/binary.rs (work_merge_candidate_rebases_when_target_advanced)

### B132

WHEN the rebase agent encounters one or more conflicts,
THE SYSTEM SHALL provide the agent with the conflicting files' content
and the conflict markers, and SHALL allow the agent to resolve those
conflicts in-place, mark them resolved with `git add`, and continue the
rebase to completion.
Test: tests/binary.rs (work_merge_rebase_resolves_trivial_conflict)

### B133

IF the rebase agent reports it cannot resolve the conflicts,
THEN THE SYSTEM SHALL transition the Merge Candidate to `needs-user`,
attach the conflict context to the rebase Task's artifact directory, and
exit without modifying the target branch.
Test: tests/binary.rs (work_merge_rebase_gives_up_transitions_to_needs_user)
Test: tests/binary.rs (work_merge_candidate_rebase_failure_leaves_target_unchanged)

### B134

IF the rebase agent exits non-zero without writing `give-up.md`,
THEN THE SYSTEM SHALL mark the rebase Task as `failed`, transition the
Merge Candidate to `failed`, and exit without modifying the target
branch.
Test: tests/binary.rs (work_merge_rebase_agent_crash_without_give_up_fails)

### B135

WHEN the rebase agent completes the rebase successfully,
THE SYSTEM SHALL set the new candidate-tip SHA on the Merge Candidate
(`candidate_commit`), on the latest completed Write Task's `output.commit`,
and on that Writer's Attempt artifact reference. Earlier Writer outputs and
their matching artifacts SHALL preserve their original provenance, including
verified no-change identity.
Test: tests/binary.rs (work_merge_candidate_lands_after_historical_no_change_writer)
Test: src/work_merge_executor.rs (regenerate_provenance_preserves_historical_no_change_writer)
Test: src/work_merge_executor.rs (regenerate_provenance_leaves_non_write_tasks_unchanged)

### B136

WHEN the rebase agent finishes resolving conflicts and before committing
each resolution,
THE SYSTEM SHALL NOT invoke project hooks (e.g., format, lint).
Post-rebase cleanup of the candidate state remains the responsibility of
`fix-pre-merge`.
Untestable: Requires LLM rebase agent resolving real conflicts to verify no hook invocation

### B137

WHEN the rebase step completes successfully,
THE SYSTEM SHALL run `check-pre-merge` and any applicable `fix-pre-merge`,
verify the resulting exact candidate through the host-owned Tester boundary,
and SHALL NOT create review or Attempt Tester Tasks before fast-forward.
Test: tests/binary.rs (work_merge_candidate_rebased_tester_failure_records_retry_evidence)
Test: tests/binary.rs (work_merge_candidate_rebased_tester_verifies_fix_pre_merge_tip)

### B138

WHEN the rebase Task or a subsequent merge step fails and the user
resolves the underlying issue, then re-runs `fluent merge-candidate land` for the
same Merge Candidate,
THE SYSTEM SHALL re-run the rebase step from the candidate workspace in
its current state and SHALL NOT reject the candidate solely because
earlier provenance regeneration failed before changing the target branch.
Test: tests/binary.rs (work_merge_candidate_retries_same_candidate_after_provenance_failure)
Test: src/work_merge_executor.rs (next_rebase_task_id_increments)

### B139

IF the target branch moves while merge checks or exact Tester verification run but
before the fast-forward merge,
THEN THE SYSTEM SHALL reject the merge, preserve the moved target branch,
and record merge status `failed` with a failure reason on the stored
Merge Candidate.
Test: tests/binary.rs (work_merge_candidate_rebased_tester_rejects_target_movement)

### B140

WHEN `fluent merge-candidate land <work-item-id> <merge-candidate-id>` lands a
Merge Candidate,
THE SYSTEM SHALL record merge status `merged`, the merged commit, and
pre-merge check and exact Tester artifacts on the stored Merge Candidate, then remove the
managed candidate worktree. If worktree cleanup fails after merging, the
system shall warn without changing the merged merge state.
Test: tests/binary.rs (work_merge_candidate_rebased_tester_pass_continues_to_land)

The former B141–B144 merge-time review and corrective Writer requirements are
retired. Failed deterministic landing verification retains the candidate and
its evidence for repair and a later land invocation.

### B145

IF configured pre-merge checks fail while `fluent merge-candidate land
<work-item-id> <merge-candidate-id>` executes,
THEN THE SYSTEM SHALL leave the target branch unchanged and record merge
status `failed`, a failure reason, and check artifacts on the stored
Merge Candidate.
Test: tests/binary.rs (work_merge_candidate_failed_check_leaves_target_unchanged)

### B146

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

### B147

WHEN every reviewer in the next planned review round receives the same
role's `review.md` from the prior completed round as an input
artifact,
THE SYSTEM SHALL render that prior review in the reviewer's prompt
framed as "a previous review of this candidate" and instruct the
reviewer to report a `Progress:` field (`yes`, `no`, `partial`, or
`first-pass`) alongside its `Verdict:`.
Test: tests/binary.rs (work_task_run_completes_review_task_with_fail_verdict_artifact)

### B148

WHEN the Attempt loop sees that every completed review in each of the
last `FLUENT_MAX_NO_PROGRESS_ROUNDS` consecutive review rounds
reported `Progress: no`,
THE SYSTEM SHALL mark the Attempt as `needs-user`, write a durable
handoff naming the failed review artifacts and the consecutive
no-progress streak, and SHALL NOT plan another write round.
Untestable: Requires multiple consecutive review rounds with LLM reviewers all reporting Progress: no

### B149

WHEN the Attempt's total completed write Tasks reach
`FLUENT_MAX_TOTAL_WRITE_ROUNDS`,
THE SYSTEM SHALL mark the Attempt as `needs-user`, write the same
handoff form, and SHALL NOT plan another write round, even if no
consecutive no-progress streak has accumulated.
Test: tests/binary.rs (work_attempt_run_counts_already_planned_followup_against_budget)
Test: tests/binary.rs (work_attempt_run_plans_followup_for_failed_reviews)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop counts preplanned follow-up against budget)

### B150

WHEN no completed review artifact has a failing verdict and any completed
review artifact has an uncertain or missing verdict,
THE SYSTEM SHALL mark the Attempt as `needs-user`, mark the Attempt
review state as `uncertain`, and write a durable handoff that names the
uncertain or missing-verdict review artifacts.
Test: tests/binary.rs (work_attempt_run_marks_uncertain_reviews_needs_user)
Test: tests/binary.rs (work_attempt_run_marks_missing_verdict_needs_user)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop marks uncertain reviews needs-user)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop marks missing verdict needs-user)

### B151

IF a Task executor fails while `fluent attempt run` advances an
Attempt,
THEN THE SYSTEM SHALL leave the Work Item state written by the Task
executor intact and exit non-zero without planning later transitions.
Test: tests/binary.rs (work_attempt_run_stops_when_task_executor_fails)

### B152

IF `fluent attempt run <work-item-id> <attempt-id>` evaluates a
completed review Task whose stored `artifact_area.path` points outside
`.fluent/work/artifacts/`,
THEN THE SYSTEM SHALL exit non-zero and leave stored Work Item state
unchanged.
Test: tests/binary.rs (work_attempt_run_rejects_unmanaged_completed_review_artifact_area_path)

### B153

IF `fluent attempt run <work-item-id> <attempt-id>` is invoked
for a missing Work Item, invalid Work Item id, missing Attempt, or
terminal Attempt,
THEN THE SYSTEM SHALL exit non-zero and leave stored Work Item state
unchanged.
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop invalid or terminal request leaves state unchanged)

### B154

IF a review Task coder exits successfully but does not write `review.md`,
THEN THE SYSTEM SHALL mark the Attempt and Task as `failed` and report
that the review artifact was not written.
Test: tests/binary.rs (work_task_run_fails_review_task_without_artifact)
Test: tests/binary.rs (work_task_run_ignores_stale_review_artifact)
Test: tests/behaviors/operations/test-work-task-run.sh (review Task missing artifact fails)

### B155

IF a review Task coder exits non-zero,
THEN THE SYSTEM SHALL mark the Attempt and Task as `failed`, leave Task
output unset, and report the coder failure.
Test: tests/binary.rs (work_task_run_marks_review_task_failed_when_coder_exits_nonzero)
Test: tests/behaviors/operations/test-work-task-run.sh (review coder failure marks Task failed)

### B156

IF a review Task mutates a readable candidate workspace,
THEN THE SYSTEM SHALL restore any committed candidate `HEAD` change, mark
the Attempt and Task as `failed`, and report the candidate workspace
mutation.
Test: tests/binary.rs (work_task_run_fails_review_task_that_dirties_candidate_workspace)
Test: tests/binary.rs (work_task_run_fails_review_task_that_dirties_candidate_workspace_and_exits_nonzero)
Test: tests/binary.rs (work_task_run_fails_review_task_that_commits_to_candidate_workspace)
Test: tests/binary.rs (work_task_run_restores_committed_review_mutation_before_dirty_failure)

### B157

IF the write Task coder exits non-zero,
THEN THE SYSTEM SHALL mark the Attempt and Task as `failed`, leave Task
output unset, and report the coder failure.
Test: tests/binary.rs (work_task_run_marks_task_failed_when_coder_exits_nonzero)
Test: tests/behaviors/operations/test-work-task-run.sh (coder failure marks Task failed)

### B158

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

### B159

IF `fluent attempt create <work-item-id> <attempt-id>` is invoked for a
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

### B160

WHEN `fluent attempt list <work-item-id>` is invoked for a stored
Work Item,
THE SYSTEM SHALL print the Work Item's Attempts, each with its id and
status, to stdout.
Test: tests/binary.rs (attempt_list_prints_attempts)

### B161

WHEN `fluent attempt show <work-item-id> <attempt-id>` is invoked for
a stored Work Item,
THE SYSTEM SHALL print that Attempt as JSON, including its tasks and
status.
Test: tests/binary.rs (attempt_show_prints_attempt_json)

### B162

WHEN `fluent merge-candidate list <work-item-id>` is invoked for a
stored Work Item,
THE SYSTEM SHALL print the Work Item's Merge Candidates, each with its
id, review state, and merge status, to stdout.
Test: tests/binary.rs (merge_candidate_list_prints_candidates)

### B163

WHEN `fluent task list <work-item-id> <attempt-id>` is invoked for a
stored Work Item and Attempt,
THE SYSTEM SHALL print the Attempt's Tasks, each with its id, kind, and
status, to stdout.
Test: tests/binary.rs (task_list_prints_tasks)

### B164

WHEN `fluent task show <work-item-id> <attempt-id> <task-id>` is
invoked for a stored Work Item, Attempt, and Task,
THE SYSTEM SHALL print that Task as JSON.
Test: tests/binary.rs (task_show_prints_task_json)

### B165

WHEN `fluent task run <work-item-id> <attempt-id> <task-id>` or
`fluent attempt run <work-item-id> <attempt-id>` launches a
`write` Task,
THE SYSTEM SHALL direct the writer to orient to the codebase before
implementing — skim the existing code around the planned changes,
match its structure, naming, error handling, and idioms, and follow
the project's stated conventions in its `AGENTS.md` / `CLAUDE.md`.
Test: src/work_task_executor.rs (write_user_prompt_directs_codebase_orientation)

### B166

WHEN `fluent task run <work-item-id> <attempt-id> <task-id>` or
`fluent attempt run <work-item-id> <attempt-id>` launches a
`write` Task,
THE SYSTEM SHALL identify subject length and style, body presence, wrapping,
formatting, and literal `\n` text conventions from `AGENTS.md` / `CLAUDE.md`
as presentation advice and SHALL NOT request corrective work solely for that
advice, while continuing to require valid history, identity, message encoding,
and attribution, including the `Co-Authored-By` prohibition.
Test: src/work_task_executor.rs (write_user_prompt_directs_advisory_commit_presentation)

### B166a

WHEN Fluent instructs a reviewer to check Factory's commit conventions, THE
SYSTEM SHALL identify subject length and style, body presence, wrapping,
formatting, and literal `\n` text as presentation advice and SHALL NOT request
corrective work or a failing verdict solely for that advice, while continuing
to flag invalid history, identity, message encoding, and attribution, including prohibited
`Co-Authored-By` trailers.
Test: src/work_task_executor.rs (documentation_review_treats_commit_presentation_as_advice)

## Atomic state-file writes

### B1

WHEN the system writes a state file (Work Item, Attempt, Task,
Merge Candidate, review state, tester results, coder info,
expertise, skills, or planning section),
THE SYSTEM SHALL ensure that a concurrent reader never observes
empty or partial file content.
Test: src/atomic_write.rs (concurrent_reader_never_sees_empty_or_partial)

### B2

IF a state-file write fails after the target file already exists,
THEN THE SYSTEM SHALL preserve the existing file contents intact.
Test: src/atomic_write.rs (failed_write_preserves_existing_contents)

### B3

IF a state-file write fails before the target file exists,
THEN THE SYSTEM SHALL leave no partial target file on disk.
Test: src/atomic_write.rs (failed_first_write_leaves_no_partial_target)

## Task execution liveness

### B1

WHEN `fluent status` includes a task that is recorded as `executing`
but whose lease no live runner holds (stale executing),
THE SYSTEM SHALL report it as interrupted, not as executing.
Test: tests/binary.rs (status_shows_stale_executing_task_as_interrupted)

### B2

WHEN the attempt loop encounters a stale-executing task,
THE SYSTEM SHALL reclaim the task and continue advancing the attempt,
rather than failing with "cannot be advanced."
Test: tests/binary.rs (attempt_run_reclaims_stale_executing_task_and_advances)

### B3

WHILE a live runner holds a task's lease,
THE SYSTEM SHALL treat the task as executing and SHALL NOT reclaim or
re-drive it.
Test: tests/binary.rs (attempt_run_refuses_to_advance_when_lease_is_held)

### B4

WHEN `fluent work-item abandon <id>` is invoked for a Work Item whose
only blocking task is stale executing (recorded `executing` but no live
runner holds its lease),
THE SYSTEM SHALL abandon the Work Item rather than reject the
abandonment.
Test: tests/binary.rs (abandon_succeeds_when_executing_task_has_no_lease)

## Coder transient failures

### B1

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

### B2

WHEN the session loop encounters a parsed `RateLimitInfo`,
THE SYSTEM SHALL wait until `retry_at` plus a per-run randomized
jitter (uniform in `[0, JITTER_MAX_SECONDS]`, default 30) before
retrying.
Test: src/coder.rs (jitter_tests::jitter_respects_max)
Test: src/coder.rs (jitter_tests::jitter_returns_zero_when_max_is_zero)
Test: src/coder.rs (jitter_tests::jitter_respects_custom_max)

### B3

WHEN multiple concurrent Fluent runs encounter rate limits with
the same `retry_at`,
THE SYSTEM SHALL apply each run's independent jitter so the runs
fan out instead of retrying at the same instant.
Test: src/coder.rs (jitter_tests::jitter_respects_max)
Test: src/coder.rs (jitter_tests::jitter_respects_custom_max)

### B4

IF a rate-limit response does not include a retry-after hint, or the
hint is unparseable,
THEN THE SYSTEM SHALL fall back to a conservative default wait
(`FLUENT_RATE_LIMIT_RETRY_AFTER_SECS`, default 1800) plus jitter —
matching previous behavior on this path.
Test: src/coder.rs (rate_limit_parsing_tests::claude_code_returns_none_for_no_timing)
Test: src/coder.rs (rate_limit_parsing_tests::claude_code_returns_none_for_unstructured_transcript)
Test: src/coder.rs (transcript_rate_limit_tests::detects_session_limit_marker)
Test: src/coder.rs (transcript_rate_limit_tests::detects_generic_rate_limit_phrase)
Test: src/coder.rs (transcript_rate_limit_tests::no_marker_returns_false)

### B5

WHEN a run transitions into rate-limit state (the first time the
session loop pauses for a parsed `retry_at` after a previously
non-rate-limited request),
THE SYSTEM SHALL fire a notification once via `notify()` (stderr log)
stating that the run paused and naming the expected resume time.
Test: src/coder.rs (rate_limit_state_tests::normal_to_rate_limited_fires_enter_notification)
Test: src/coder.rs (rate_limit_state_tests::full_cycle_fires_enter_once_and_leave_once)

### B6

WHEN a run transitions out of rate-limit state (the first successful
non-rate-limited request after a paused wait),
THE SYSTEM SHALL fire a notification once stating that the run
resumed.
Test: src/coder.rs (rate_limit_state_tests::full_cycle_fires_enter_once_and_leave_once)

### B7

WHEN the session loop retries within an ongoing rate-limit pause
(multiple retries against the same `retry_at` because the provider
returned another 429),
THE SYSTEM SHALL NOT fire additional enter-state notifications;
notifications fire on state transitions, not on each retry tick.
Test: src/coder.rs (rate_limit_state_tests::rate_limited_to_rate_limited_does_not_refire_notification)
Test: src/coder.rs (rate_limit_state_tests::full_cycle_fires_enter_once_and_leave_once)

### B8

WHEN the Coder abstraction is queried for rate-limit parsing,
THE SYSTEM SHALL provide a parser for the Anthropic provider and a
parser for the Codex provider, both returning
`Option<RateLimitInfo>` from a provider-specific response shape.
Test: src/coder.rs (rate_limit_parsing_tests::fixture_claude_code_retry_after)
Test: src/coder.rs (rate_limit_parsing_tests::fixture_codex_retry_after)
Test: src/coder.rs (rate_limit_parsing_tests::codex_parses_reset_at_iso8601)
Test: src/coder.rs (rate_limit_parsing_tests::fixture_codex_reset_at)

### B9

WHEN no transcript file is configured for a Coder invocation,
THE SYSTEM SHALL propagate the original exit code without rate-
limit retry, since transient failure cannot be detected without
the transcript content.
Untestable: Negative code path; all production callers configure a transcript

## Review verdict reliability

### B1

WHEN a review would base a `Fail` verdict on a removal claim,
THE SYSTEM SHALL confirm the diff actually removes the cited content,
and SHALL NOT `Fail` on a removal claim the diff does not support.
Test: tests/behaviors/skills/review-verdict-rules.sh

### B2

WHEN resolving a review finding requires a design decision that the
brief, behaviors, or approach do not settle,
THE SYSTEM SHALL return `Uncertain` rather than `Fail`.
Test: tests/behaviors/skills/review-verdict-rules.sh

---

## Task error resilience

### B1

WHEN a task coder error occurs and the error resolves within the
bounded retry budget,
THE SYSTEM SHALL recover automatically and complete the task.
Test: tests/binary.rs (work_task_run_recovers_review_task_when_coder_succeeds_on_retry)
Test: tests/binary.rs (work_task_run_tester_recovers_when_error_is_transient)

### B2

WHEN a task coder error persists beyond the bounded retry budget,
THE SYSTEM SHALL pause the Attempt at `needs-user` rather than marking
it `failed`.
Test: tests/binary.rs (work_task_run_persistent_coder_error_pauses_attempt_at_needs_user)
Test: tests/binary.rs (work_task_run_pauses_attempt_when_review_coder_exits_nonzero)
Test: tests/binary.rs (work_task_run_pauses_attempt_when_write_coder_exits_nonzero)
Test: tests/binary.rs (work_task_run_tester_persistent_error_pauses_attempt_at_needs_user)

### B3

WHEN a task coder error is an auth rejection,
THE SYSTEM SHALL pause the Attempt at `needs-user` without retrying the
task.
Test: tests/binary.rs (work_task_run_auth_rejection_pauses_attempt_after_one_retry)
Test: tests/binary.rs (work_task_run_write_auth_rejection_pauses_attempt_after_one_retry)

### B4

WHEN a task coder error is an auth rejection,
THE SYSTEM SHALL write a handoff that instructs the user to
re-authenticate and resume.
Test: tests/binary.rs (work_task_run_auth_rejection_pauses_attempt_after_one_retry)
Test: tests/binary.rs (work_task_run_write_auth_rejection_pauses_attempt_after_one_retry)

---

## Brief capture

### B1

WHEN the user invokes the capture-brief skill,
THE SYSTEM SHALL interview the user, research the codebase, and write
a brief for a Work Item.
Test: tests/behaviors/skills/code-reviewer.md (test-skill)
Test: tests/behaviors/operations/test-planning-skills-work-context.sh

### B2

WHEN the user invokes the fluent skill for new delegated
build work,
THE SYSTEM SHALL teach Work Items, Attempts, Tasks, Workspaces, and Merge
Candidates as the lifecycle, direct the user through Work Item creation,
Attempt execution, Merge Candidate inspection, and `fluent merge-candidate land`.
Test: tests/behaviors/operations/test-build-in-fluent-work-model-guidance.sh

### B3

WHEN the brief is confirmed by the user,
THE SYSTEM SHALL keep the approved brief available for later planning.
Untestable: Requires interactive user confirmation in a conversation session

## Behavior definition

### B1

WHEN the user invokes the define-behaviors skill,
THE SYSTEM SHALL read the brief and existing behaviors, elaborate into
EARS-format behavioral statements, and write behaviors.diff.md.
Test: tests/behaviors/skills/run-summary-behaviors.md (test-skill)

### B2

WHEN behaviors are approved by the user,
THE SYSTEM SHALL keep the behavior diff available for Work Item planning
context.
Test: tests/behaviors/skills/run-summary-behaviors.md (test-skill)

## Approach design

### B1

WHEN the user invokes the design-approach skill,
THE SYSTEM SHALL research external systems, evaluate options, and write
approach.md with relevant expertise references, key technical decisions,
and solution direction.
Untestable: Requires interactive LLM skill invocation with codebase research

### B2

WHEN the approach is approved by the user,
THE SYSTEM SHALL keep the approach available for Work Item planning
context.
Untestable: Requires interactive user approval in a conversation session

## Execution planning

### B1

WHEN the user invokes the plan-execution skill,
THE SYSTEM SHALL break the approach into executable Work Item steps,
describe one Work Item with an Attempt and Task notes or peer Work Items
as the default planning units, and write plan.md.
Test: tests/behaviors/skills/format-check-plan.md (test-skill)
Test: tests/behaviors/operations/test-planning-skills-work-context.sh

### B2

WHEN the plan is approved by the user,
THE SYSTEM SHALL create the Work Item with approved planning context.
Test: tests/behaviors/operations/test-planning-skills-work-context.sh

### B3

WHEN the plan-execution skill describes parallel execution,
THE SYSTEM SHALL describe peer Work Items first and keep Attempt and Task
sequencing as planning notes rather than executable dependencies.
Test: tests/behaviors/skills/parallel-work-items-plan.md (test-skill)
Test: tests/behaviors/operations/test-planning-skills-work-context.sh

### B4

WHEN interactive planning begins, the request is grounded enough to explain the
choice, and the user has not already chosen a collaboration style, THE SYSTEM
SHALL offer once that the user may say `use your judgment through the rest of
planning`, explain the single final planning-set confirmation and mandatory
interruption boundaries, and avoid repeating the offer after the user chooses a
collaboration style.
Test: tests/behaviors/operations/test-planning-wide-delegation.sh (planning delegation is offered early)

### B5

WHEN the user explicitly delegates judgment through the rest of planning,
THE SYSTEM SHALL keep that delegation active through the remaining applicable
planning stages until the user revokes it or completes the final planning
confirmation; `keep going`, silence, a saved draft, or execution autonomy SHALL
NOT activate it.
Test: tests/behaviors/operations/test-planning-wide-delegation.sh (explicit delegation spans the remaining planning stages)

### B6

WHILE planning-wide delegation is active, THE SYSTEM SHALL make grounded
recommendation-level choices, record their assumptions and tradeoffs, keep each
intermediate planning artifact provisional, and advance without intermediate
artifact confirmations or recommendation-level questions.
Test: tests/behaviors/operations/test-planning-wide-delegation.sh (explicit delegation spans the remaining planning stages)

### B7

WHILE planning-wide delegation is active, IF a choice would change a stated or
accepted behavior or scope, make a consequential or difficult-to-reverse
tradeoff, contradict a recorded project decision, require missing information or
access, cross outside planning, or invoke an exceptional policy, THEN THE SYSTEM
SHALL interrupt with one focused user decision and continue delegation afterward
unless the user revokes it.
Test: tests/behaviors/operations/test-planning-wide-delegation.sh (delegated planning keeps mandatory interruptions)

### B8

WHEN delegated planning reaches its last applicable artifact, THE SYSTEM SHALL
show the complete planning set, the consequential choices made under delegation,
every proposed Work Item and Learner mode, and any release criteria; ask one
confirmation; and, after a revision request, update every affected downstream
artifact before presenting the complete set again.
Test: tests/behaviors/operations/test-planning-wide-delegation.sh (delegated planning uses one final planning-set confirmation)

### B9

WHEN the user delegates planning judgment without confirming the completed
planning set, THE SYSTEM SHALL NOT create a Work Item or Attempt, execute work,
authorize exceptional scope, or land a candidate; final planning confirmation
may authorize only the displayed Work Item creation commands.
Test: tests/behaviors/operations/test-planning-wide-delegation.sh (delegated planning does not authorize execution)

### B10

WHEN the user revokes planning-wide delegation before final confirmation,
THE SYSTEM SHALL return to the earliest provisional artifact, present each
unapproved artifact through the normal ordered confirmation gates, and SHALL NOT
promote a provisional artifact to approved because delegation ended.
Test: tests/behaviors/operations/test-planning-wide-delegation.sh (revoking delegation restores ordered confirmations)

## Fargate teardown

### B1

WHEN `fluent fargate teardown` is invoked,
THE SYSTEM SHALL delete the CloudFormation stack used by the Fargate
runtime, wait for stack deletion to reach a terminal state, and
report the deletion outcome.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)

### B2

WHEN `fluent fargate teardown` is invoked without `--keep-ecr`,
THE SYSTEM SHALL delete the ECR repository created by the bootstrap.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)

### B3

WHEN `fluent fargate teardown` is invoked without `--keep-s3`,
THE SYSTEM SHALL empty and delete the S3 bucket created by the
bootstrap.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)

### B4

WHEN `fluent fargate teardown` is invoked with `--keep-ecr`,
THE SYSTEM SHALL leave the ECR repository intact while still deleting
the CloudFormation stack and the state file.
Test: tests/binary.rs (fargate_teardown_keep_ecr_skips_ecr_delete)

### B5

WHEN `fluent fargate teardown` is invoked with `--keep-s3`,
THE SYSTEM SHALL leave the S3 bucket intact while still deleting the
CloudFormation stack and the state file.
Test: tests/binary.rs (fargate_teardown_keep_s3_skips_s3_delete)

### B6

WHEN `fluent fargate teardown` completes its destructive steps,
THE SYSTEM SHALL delete `~/.config/fluent/fargate.state.json` so the
next `--runtime fargate` invocation triggers a fresh `ensure_setup`.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)

### B7

IF `fluent fargate teardown` is invoked when no state file exists
and no CloudFormation stack is present,
THEN THE SYSTEM SHALL exit zero with a message saying nothing
needed teardown.
Test: tests/binary.rs (fargate_teardown_nothing_to_teardown)

### B8

IF a destructive step fails (CloudFormation, ECR, or S3),
THEN THE SYSTEM SHALL print the error, exit non-zero, and leave the
state file in place so a retry resumes from the failed step.
Test: tests/binary.rs (fargate_teardown_error_preserves_state_file)

### B9

WHEN `fluent fargate teardown` completes successfully,
THE SYSTEM SHALL print a one-line summary listing what was removed
and what was kept.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)
Test: src/fargate_bootstrap.rs (teardown_outcome_display_all_removed)
Test: src/fargate_bootstrap.rs (teardown_outcome_display_partial_keep_ecr)
Test: src/fargate_bootstrap.rs (teardown_outcome_display_partial_keep_s3)

## Fargate Codex support

### B1

WHEN `fluent attempt run <work-id> --runtime fargate --coder
codex` is invoked,
THE SYSTEM SHALL launch an ECS task using the Fluent base image,
pass `FLUENT_CODER=codex` plus the Codex auth env var as task
overrides, and run the Attempt to completion producing the same
artifact shape that the local Codex path produces today.
Test: src/fargate.rs (codex_overrides_include_auth_json_and_fluent_coder)

### B2

WHEN `fluent merge-candidate land <work-id> --runtime fargate --coder codex`
is invoked,
THE SYSTEM SHALL launch an ECS task that runs the agentic rebase
step using Codex, then proceeds through merge checks, reviews, and
fast-forward unchanged from the Claude path.
Test: src/fargate.rs (codex_overrides_include_auth_json_and_fluent_coder)

### B3

WHEN the Fargate entrypoint runs with `FLUENT_CODER=codex`,
THE SYSTEM SHALL require the `CODEX_AUTH_JSON` env var, write its
contents to `${HOME}/.codex/auth.json` with mode 0600 before
invoking the fluent binary, and exit non-zero with a clear error
when the env var is missing.
Test: tests/behaviors/operations/test-fargate-entrypoint-codex.sh (codex writes auth.json and unsets OPENAI_API_KEY, codex missing CODEX_AUTH_JSON fails)

### B4

WHEN the Fargate entrypoint runs with `FLUENT_CODER=claude` or
`FLUENT_CODER` unset,
THE SYSTEM SHALL require the `CLAUDE_CODE_OAUTH_TOKEN` env var
(today's behavior, unchanged) and SHALL NOT require any Codex
credentials.
Test: tests/behaviors/operations/test-fargate-entrypoint-codex.sh (claude path unchanged, default coder is claude)

### B5

WHEN the Fluent base image is built,
THE SYSTEM SHALL install both `@anthropic-ai/claude-code` and
`@openai/codex` via npm such that `claude` and `codex` are both on
the `PATH` of the runtime container user.
Test: infrastructure/run/Dockerfile (visual inspection)

### B6

WHEN ECS task launch occurs from a Fargate runtime path,
THE SYSTEM SHALL set `FLUENT_CODER` as a task override and pass the
appropriate auth env var (`CLAUDE_CODE_OAUTH_TOKEN` for Claude,
`CODEX_AUTH_JSON` for Codex) without logging, persisting, or echoing
the value into user-facing output.
Test: src/fargate.rs (claude_overrides_include_oauth_token_and_fluent_coder, codex_overrides_include_auth_json_and_fluent_coder)

### B7

WHEN the host machine launches a Fargate Codex run,
THE SYSTEM SHALL read the Codex auth from `~/.codex/auth.json` on
the host and pass its contents as the `CODEX_AUTH_JSON` task
override, returning a clear error when the host auth file is
missing or unreadable.
Test: src/fargate.rs (codex_overrides_err_when_host_auth_file_missing)

### B8

WHEN the host launches a Fargate Codex run and the host
`~/.codex/auth.json` has `auth_mode != "chatgpt"`,
THE SYSTEM SHALL refuse to launch with a clear error stating that
Fargate Codex requires ChatGPT subscription auth, and SHALL NOT
issue the ECS RunTask call.
Test: src/fargate.rs (codex_overrides_err_when_host_auth_mode_is_apikey)

### B9

WHEN the Fargate entrypoint runs with `FLUENT_CODER=codex`,
THE SYSTEM SHALL `unset OPENAI_API_KEY` in the entrypoint's
exported environment before invoking the fluent binary, so any
accidental env-var leak cannot flip the run from ChatGPT subscription
billing to per-token API billing.
Test: tests/behaviors/operations/test-fargate-entrypoint-codex.sh (codex OPENAI_API_KEY unset in binary env)

### B10

IF the project's `.fluent/Dockerfile` (or any base image layer)
ships a `~/.codex/config.toml` containing
`preferred_auth_method = "apikey"`,
THEN the Fargate Codex entrypoint SHALL detect that setting and
exit non-zero with an error explaining that Fargate Codex enforces
ChatGPT subscription billing.
Test: tests/behaviors/operations/test-fargate-entrypoint-codex.sh (codex config.toml apikey preference rejected)

### B11

IF the Fluent base image is built without the Codex npm package,
THEN the Fargate Codex entrypoint check SHALL exit non-zero with
"codex binary not found on PATH" before any work is attempted.
Test: tests/behaviors/operations/test-fargate-entrypoint-codex.sh (codex missing CODEX_AUTH_JSON fails — env var check fires first; binary check fires after)

---

## Status reporting

### B6

WHEN status, Work Item inspection, or dashboard observes a merged candidate
after its source checkout is retired, THE SYSTEM SHALL show its recorded landed
result and retained Tester evidence accepted for the exact merged commit,
including when Learning or rebase changed the candidate after the Attempt
Tester ran. It SHALL validate the retained landing result and target baseline
against their recorded identity, command occurrence, manifest, and landing
intent. An earlier commit's Tester result cannot supply final proof.
Test: tests/binary.rs (landed_status_preserves_result_after_source_retirement, landed_status_uses_final_landing_tester_evidence, landed_status_validates_retained_landing_evidence)

### B7

WHEN those views present verification for merged Work, THE SYSTEM SHALL
identify retained Tester results as evidence history and SHALL NOT present
historical Writer rows as current requirements.
Test: tests/binary.rs (landed_status_does_not_reopen_historical_requirements)

### B8

IF retained landed Tester evidence is missing or inconsistent, THEN THE SYSTEM
SHALL report an evidence-history diagnostic without offering a pre-land resume
command.
Test: tests/binary.rs (landed_status_reports_unavailable_historical_evidence)

### B9

WHEN an unmerged candidate is inspected, including a Complete Attempt still
awaiting landing, THE SYSTEM SHALL retain current recovery and verification
checks.
Test: tests/binary.rs (landed_status_keeps_unmerged_checks_strict)

### B10

WHEN these observation routes inspect Work, THE SYSTEM SHALL preserve stored
records and retained evidence bytes.
Test: tests/binary.rs (landed_status_observation_preserves_recorded_bytes, landed_status_observation_preserves_recorded_bytes_with_pending_publication)

### B1

WHEN `fluent status` is invoked and stored Work Items exist,
THE SYSTEM SHALL display a Work Items section with each Work Item's
latest Attempt, selected Task, review state, Merge Candidate, merge
state, actionable label, and title.
Test: tests/binary.rs (status_shows_work_items_without_runs)

### B2

WHEN `fluent status` lists an abandoned Work Item before cleanup,
THE SYSTEM SHALL surface it as terminal abandoned Work rather than as
Work that still needs human planning input.
Test: src/work_status.rs (summarize_abandoned_work_item_shows_terminal_action)
Test: tests/behaviors/operations/test-work-status-dashboard.sh (status surfaces abandoned Work as terminal)

### B3

WHEN `fluent status` is invoked for a project with Work Items,
THE SYSTEM SHALL display the Work Items section.
Test: tests/binary.rs (status_shows_work_items_without_runs)

### B4

WHEN `fluent status` is invoked for a project with no Work Items and no
Work Item read errors,
THE SYSTEM SHALL report that no Work Items were found.
Test: tests/binary.rs (status_no_fluent_dir)

### B5

WHEN `fluent status` reads one or more invalid Work Item files,
THE SYSTEM SHALL report the invalid Work model path in a Work Item read
errors section while still displaying valid Work Items.
Test: tests/behaviors/operations/test-work-status-dashboard.sh (status reports invalid Work without hiding valid state)

## Cleanup

### B1

WHEN `fluent cleanup` sees eligible terminal Work Items,
THE SYSTEM SHALL report Work Item state, artifacts, managed candidate
worktrees, and Work branches without removing them unless `--apply` is
passed.
Test: tests/binary.rs (cleanup_work_items_dry_run_and_apply_manage_state_worktree_and_branch), tests/binary.rs (cleanup_work_items_removes_terminal_merge_candidate_artifacts_and_worktree)

### B2

WHEN `fluent cleanup --apply` cleans an eligible terminal Work Item,
THE SYSTEM SHALL remove the Work Item state, referenced managed Work
artifacts, registered managed candidate worktrees, and Work branches.
The cleanup SHALL also remove any Fargate runtime metadata directories
recorded under `.fluent/work/runtime/attempts/<work-item-id>/` and
`.fluent/work/runtime/merges/<work-item-id>/`.
Test: tests/binary.rs (cleanup_work_items_dry_run_and_apply_manage_state_worktree_and_branch), tests/binary.rs (cleanup_work_items_removes_terminal_merge_candidate_artifacts_and_worktree), src/cleanup.rs (terminal_work_item_cleanup_removes_runtime_arn_dirs)

### B3

WHEN `fluent cleanup` sees an abandoned Work Item with no executing or
reviewing Attempts, no executing Tasks, no reviewing Merge Candidates,
and no executing Merge Candidate merges,
THE SYSTEM SHALL select it for cleanup when its owned source is clean and
integrated or no owned source remains, including its managed sibling
worktree, Work branch, state records, and Work artifacts.
Test: tests/behaviors/operations/test-cleanup.sh (cleanup selects abandoned needs-user Work Items)
Test: tests/behaviors/operations/test-cleanup.sh (cleanup skips abandoned Work with reviewing Attempt)
Test: tests/behaviors/operations/test-cleanup.sh (cleanup skips abandoned Work with active Merge Candidate)

### B4

WHEN a lifecycle mutation or `fluent work-item upgrade` processes stored Work
with older artifact references under `.fluent/work/artifacts/<attempt-id>/...`,
THE SYSTEM SHALL expose those references under
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/...` and move
existing older artifacts into that namespace when no namespaced artifact
already exists.
Test: src/work_model.rs (explicit_upgrade_owns_legacy_artifact_migration)

### B5

WHEN Work cleanup sees artifact references that are absolute paths, use
parent escapes, or do not resolve under `.fluent/work/artifacts/`,
THE SYSTEM SHALL ignore those unmanaged artifact references without
reporting or removing them.
Test: tests/behaviors/operations/test-cleanup.sh (Work cleanup ignores unmanaged artifacts)

### B6

WHEN Work cleanup sees active Work Items, Work Items with active Merge
Candidates, or Work Items needed to finish a required recovery operation,
THE SYSTEM SHALL leave them out of cleanup results.
An optional future retry of terminal failed Work does not by itself require
retention.
Test: tests/binary.rs (cleanup_work_items_dry_run_and_apply_manage_state_worktree_and_branch), tests/binary.rs (cleanup_work_items_selects_failed_terminal_and_skips_pending_merge_candidate), tests/binary.rs (workflow_cleanup_retains_pending_recovery)

### B7

WHEN Work cleanup sees failed terminal Work Items with clean, integrated source
or no remaining source assets,
THE SYSTEM SHALL select them for cleanup.
Test: tests/binary.rs (cleanup_work_items_selects_failed_terminal_and_skips_pending_merge_candidate)

### B8

WHEN `fluent cleanup` sees a top-level
`.fluent/work/artifacts/<work-item-id>/` directory without a matching
stored Work Item,
THE SYSTEM SHALL report the orphaned Work artifact root without removing
it unless `--apply` is passed.
Test: tests/binary.rs (cleanup_work_items_reports_and_removes_orphan_artifact_roots), tests/behaviors/operations/test-cleanup.sh (cleanup removes orphan Work artifact roots)

### B9

WHEN `fluent cleanup --apply` sees a top-level
`.fluent/work/artifacts/<work-item-id>/` directory without a matching
stored Work Item,
THE SYSTEM SHALL remove the orphaned Work artifact root and report that
it was removed.
Test: tests/binary.rs (cleanup_work_items_reports_and_removes_orphan_artifact_roots), tests/behaviors/operations/test-cleanup.sh (cleanup removes orphan Work artifact roots)

### B10

WHEN Work cleanup sees top-level entries under `.fluent/work/artifacts/`
that are files or directories with matching stored Work Items,
THE SYSTEM SHALL ignore them for orphan Work artifact cleanup.
Test: tests/binary.rs (cleanup_work_items_reports_and_removes_orphan_artifact_roots), tests/behaviors/operations/test-cleanup.sh (cleanup removes orphan Work artifact roots)

### B11

WHEN Work cleanup selects a candidate for removal, THE SYSTEM SHALL recheck
its current eligibility and keep that eligibility protected through removal
of its managed worktrees, branches, artifacts, and state.
Test: tests/binary.rs (workflow_cleanup_rechecks_current_eligibility), tests/binary.rs (workflow_cleanup_holds_eligibility_through_removal)

### B12

WHEN an otherwise eligible Work Item has unmerged source commits or pending
file changes in a managed candidate worktree, THE SYSTEM SHALL retain its
state, artifacts, worktrees, and branches and report the affected source in
preview and apply results.
Test: tests/binary.rs (cleanup_retains_unmerged_source_across_attempts), tests/binary.rs (cleanup_preserves_earlier_integrated_source_when_later_source_is_unmerged), tests/binary.rs (cleanup_retains_pending_file_changes)

### B13

IF cleanup cannot establish an existing source asset's identity, cleanliness,
target branch, or integration status, THE SYSTEM SHALL retain the whole Work
Item and report the reason.
Test: tests/binary.rs (cleanup_retains_unverifiable_source), tests/binary.rs (cleanup_retains_unmerged_head_in_missing_registered_worktree), tests/binary.rs (cleanup_reports_retained_source_and_cleans_safe_work)

### B14

WHEN cleanup applies a selected Work Item's removal, THE SYSTEM SHALL recheck
its source before deletion and retain the whole Work Item if that source has
changed. IF source changes during removal, THE SYSTEM SHALL refuse remaining
removals.
Test: tests/binary.rs (cleanup_rechecks_source_before_removal), tests/binary.rs (cleanup_refuses_late_source_changes)

## Dashboard

### B1

WHEN `fluent dashboard` is invoked,
THE SYSTEM SHALL open a read-only Current Work console. It groups non-terminal
Work as Needs you, Running, and Ready, in that order, and shows a count for each
non-empty group. Unknown current actions remain visible in Needs you.
Test: dashboard::tests::dashboard_opens_on_current_work,
dashboard::tests::current_work_is_grouped_with_counts,
dashboard::tests::unknown_current_action_remains_visible

### B2

WHEN the operator presses `a`,
THE SYSTEM SHALL toggle All Work, which retains Current Work groups and adds a
Terminal group for complete, merged, and abandoned Work. Empty Current Work
names the All Work key; an empty project has no selection.
Test: dashboard::tests::all_key_toggles_current_and_all_work,
dashboard::tests::all_work_adds_terminal_group,
dashboard::tests::current_empty_state_points_to_all_work,
dashboard::tests::empty_project_has_no_selection

### B3

WHEN Work Item detail is visible, THE SYSTEM SHALL show the selected Work Item's
identifier, title, action, Attempt, Task, review, Merge Candidate, merge state,
metrics, compatibility warnings, and release summary when those values exist.
Test: dashboard::tests::detail_shows_selected_work_state

### B4

WHEN canonical status guidance supplies a next action for the selected Work Item,
THE SYSTEM SHALL show that exact guidance in Work Item detail.
Test: dashboard::tests::detail_shows_canonical_next_action

### B5

WHEN canonical status guidance supplies no next action for the selected Work Item,
THE SYSTEM SHALL label the detail as having no operator action.
Test: dashboard::tests::detail_does_not_invent_next_action

### B6

WHEN a non-empty view opens, the console SHALL select the first Work Item. The
operator can use Up/`k` and Down/`j` to reach every row and list diagnostic;
selection survives a refresh, filtering, reordering, and terminal resize by ID,
or falls back to the nearest remaining row.
Test: dashboard::tests::initial_selection_uses_first_non_empty_section,
dashboard::tests::overflow_rows_remain_navigable,
dashboard::tests::selection_survives_refresh_reorder_and_filter,
dashboard::tests::removed_selection_uses_nearest_remaining_row

### B7

WHILE the console runs, it SHALL poll stored Work state about every two seconds
and on `r`. A successful changed refresh replaces the snapshot; a failed refresh
retains the last snapshot and shows a stale error. The list shows the count of
Work Item read errors and lets the operator scroll to every error. An unchanged
successful poll does not request a redraw.
Test: dashboard::tests::refresh_key_requests_immediate_poll,
dashboard::tests::successful_poll_replaces_dashboard_snapshot,
dashboard::tests::successful_poll_clears_stale_state,
dashboard::tests::failed_poll_keeps_snapshot_and_marks_it_stale,
dashboard::tests::work_read_errors_remain_available_with_overflow,
dashboard::tests::idle_dashboard_does_not_request_repaint

### B8

WHEN the terminal is at least 100 columns wide and 15 rows high, the console
SHALL show its grouped list and selected Work Item detail side by side. From 60
through 99 columns it SHALL show list and detail panes separately with Enter and
Esc. Below 60 columns or 15 rows it SHALL show the required minimum size and
quit control. Compact rows truncate by display width; detail wraps and scrolls.
Test: dashboard::tests::wide_layout_shows_list_and_detail,
dashboard::tests::narrow_layout_switches_between_list_and_detail,
dashboard::tests::long_and_wide_character_content_stays_within_regions,
dashboard::tests::overflow_detail_remains_navigable,
dashboard::tests::resize_preserves_selection_and_changes_layout,
dashboard::tests::undersized_terminal_shows_resize_message

## Dashboard keyboard

### B1

WHEN the user presses `q` or Ctrl+C,
THE SYSTEM SHALL exit the dashboard and restore the terminal.
Untestable: Requires live terminal restore after process exit

### B2

WHEN the operator presses `?`, THE SYSTEM SHALL show controls for the current
layout and close help with Esc or `?`. The help bar SHALL show the applicable
selection or scrolling controls. Dashboard controls do not modify stored Work
or execution state.
Test: dashboard::tests::help_lists_contextual_controls_and_closes,
dashboard::tests::help_bar_tracks_current_view,
dashboard::tests::dashboard_controls_leave_work_state_unchanged

## Per-project Fargate images

### B1

WHEN `fluent fargate ensure-setup` runs and `.fluent/Dockerfile`
does not exist in the project root,
THE SYSTEM SHALL create it as a stub containing `ARG FLUENT_BASE_URI`
and `FROM ${FLUENT_BASE_URI}` (plus a brief comment on how to extend
it), and SHALL leave the file uncommitted for the user to inspect and
version-control.
Test: src/fargate_bootstrap.rs (ensure_project_dockerfile_stub_creates_when_missing, ensure_project_dockerfile_stub_skips_when_exists)
Test: tests/binary.rs (fargate_ensure_setup_creates_dockerfile_stub_when_missing)

### B2

WHEN `fluent fargate ensure-setup` runs,
THE SYSTEM SHALL build the Fluent base image and push it to the
project's ECR repo tagged with the current Fluent version (e.g.,
`fluent-base-0.1.0`), unless an image with that tag already exists
in the repo, in which case the build is skipped.
Test: src/fargate_bootstrap.rs (base_image_tag_includes_version)
Test: tests/binary.rs (fargate_ensure_setup_skips_base_build_when_ecr_tag_exists)

### B3

WHEN `fluent fargate ensure-setup` runs,
THE SYSTEM SHALL compute the SHA-256 of the project's
`.fluent/Dockerfile`, check the project's ECR repo for the project
image tagged with that hash (e.g., `project-a3f2b8c9d4e1`), build and
push the project image if missing, and skip the build if present.
Test: src/fargate_bootstrap.rs (project_image_tag_from_hash_deterministic_12_hex, sha256_file_is_stable, sha256_file_changes_with_content)
Test: tests/binary.rs (fargate_ensure_setup_skips_project_build_when_ecr_tag_exists)

### B4

WHEN `fluent merge-candidate land --runtime fargate` runs,
THE SYSTEM SHALL launch the ECS task using the project image whose
tag matches the SHA-256 of `.fluent/Dockerfile` at launch time.
Untestable: Requires live ECS task launch with ECR project image

### B5

WHEN `fluent merge-candidate land --runtime fargate` runs and the project
image for the current `.fluent/Dockerfile` content hash does not
exist in ECR,
THE SYSTEM SHALL build and push the project image (same procedure
as bootstrap) before launching the ECS task.
Untestable: Requires ECR image absence detection and Docker build during ECS launch

### B6

WHEN a local Attempt or local post-merge review runs (no `--runtime
fargate`),
THE SYSTEM SHALL NOT consult `.fluent/Dockerfile` and SHALL NOT
build or launch any container; the user's local environment is used
as today.
Untestable: Negative behavior; local execution is the default path tested by all non-Fargate tests

### B7

WHEN `fluent fargate teardown` runs,
THE SYSTEM SHALL delete both the Fluent base image tags and the
project image tags from the project's ECR repo, in addition to the
existing teardown behaviors.
Test: tests/binary.rs (fargate_teardown_deletes_stack_ecr_s3_and_removes_state)

### B8

WHEN this repo's `.fluent/Dockerfile` is used to build the project
image,
THE SYSTEM SHALL produce an image that contains `rustc`, `cargo`,
`rustfmt`, and `clippy` such that `cargo fmt --check`, `cargo test`,
and `cargo clippy` execute successfully under the merge-check hook.
Untestable: Requires Docker image build and Rust toolchain verification inside container

### B9

IF the project's `.fluent/Dockerfile` cannot be built (syntax
error, unreachable base image, network failure during `docker
build`),
THEN `fluent fargate ensure-setup` and `fluent merge-candidate land
--runtime fargate` SHALL exit non-zero with a clear error that
names the failing build step and leaves the project's ECR repo
unchanged.
Untestable: Requires Docker build failure during Fargate bootstrap or merge

### B10

WHEN the project's `.fluent/Dockerfile` references a `FROM
<ecr-uri>/fluent-base:<version>` tag that does not exist in ECR,
THE SYSTEM SHALL surface the missing-tag error from `docker build`
to the user without retry, so the user can either bump Fluent to
match the referenced base or update the `FROM` line.
Untestable: Requires Docker build with missing ECR base image tag

## Non-interactive git defaults

### B1

WHEN Fluent invokes git through the wrapper module,
THE SYSTEM SHALL set the environment variables `GIT_EDITOR=true`,
`GIT_SEQUENCE_EDITOR=true`, and `GIT_TERMINAL_PROMPT=0` on the git
subprocess.
Test: src/git.rs (build_command_sets_non_interactive_env)

### B2

WHEN Fluent invokes git through the wrapper module,
THE SYSTEM SHALL pass `-c commit.gpgsign=false` and
`-c core.editor=true` to every git subcommand.
Test: src/git.rs (build_command_passes_gpgsign_false, build_command_passes_core_editor_true)

### B3

WHEN any Fluent code path issues a commit in a candidate worktree
(writer Task, agentic rebase, fix-pre-merge auto-commit, or any
future commit-producing path),
THE SYSTEM SHALL produce an unsigned commit regardless of the
project's global or repo-level git config.
Test: src/git.rs (build_command_passes_gpgsign_false)

### B4

WHEN the git wrapper is invoked,
THE SYSTEM SHALL capture stdout and stderr and surface non-zero exit
codes with the full command line plus captured output, so a failure
is debuggable without re-running.
Test: src/git.rs (run_returns_error_with_full_context)

### B5

IF a git operation genuinely requires user interaction (merge
conflict not resolved by the caller, credential prompt, signing
secret unlock, etc.),
THEN THE SYSTEM SHALL exit non-zero with diagnostic context and
SHALL NOT silently block the agent.
Test: src/git.rs (run_returns_error_with_full_context)

### B6

WHEN this Work Item lands,
THE SYSTEM SHALL contain zero direct `Command::new("git")` call sites
in `src/` outside the wrapper module.
Test: tests/binary.rs (no_direct_git_command_in_src)

## Per-task timestamps

### B1

WHEN Fluent creates a new Task (write, review, or rebase),
THE SYSTEM SHALL set `task.created_at` to the current UTC time in
ISO 8601 / RFC 3339 format before persisting the Task JSON.
Test: src/work_model.rs (initial_attempt_populates_created_at_timestamps)

### B2

WHEN Fluent transitions a Task out of Planned for the first time,
THE SYSTEM SHALL set `task.started_at` to the current UTC time if
it is not already set.
Test: src/work_model.rs (mark_task_started_is_idempotent)

### B3

WHEN Fluent transitions a Task to a terminal status (Complete,
Failed, or NeedsUser),
THE SYSTEM SHALL set `task.completed_at` to the current UTC time.
Test: src/work_model.rs (set_task_terminal_sets_completed_at_and_status,
set_task_terminal_is_idempotent_on_completed_at)

### B4

WHEN Fluent creates a new Attempt,
THE SYSTEM SHALL set `attempt.created_at` to the current UTC time
before persisting the Attempt JSON.
Test: src/work_model.rs (initial_attempt_populates_created_at_timestamps)

### B5

WHEN an Attempt reaches Failed, NeedsUser, or review-only Complete without a
pending Learning gate,
THE SYSTEM SHALL set `attempt.completed_at` to the current UTC time.
Test: src/work_model.rs (set_attempt_terminal_round_trip),
src/work_attempt_loop.rs (review_only_attempt_skips_learner)

### B6

WHEN Fluent creates a new MergeCandidate,
THE SYSTEM SHALL set `merge_candidate.created_at` to the current
UTC time before persisting the Merge Candidate JSON.
Test: src/work_model.rs (merge_candidate_creation_populates_created_at)

### B7

WHEN Fluent transitions a MergeCandidate's merge_state.status out
of Pending for the first time (typically to Executing),
THE SYSTEM SHALL set `merge_candidate.started_at` to the current
UTC time if it is not already set.
Test: src/work_model.rs (mark_merge_candidate_started_is_idempotent)

### B8

WHEN Fluent transitions a MergeCandidate to a terminal status
(Merged, Failed, or NeedsUser),
THE SYSTEM SHALL set `merge_candidate.completed_at` to the current
UTC time.
Test: src/work_model.rs (set_merge_candidate_terminal_round_trip)

### B9

WHEN `fluent work-item show` returns Task, Attempt, and Merge Candidate
JSON,
THE SYSTEM SHALL include the timestamp fields verbatim from their
on-disk representation.
Test: src/work_model.rs (task_with_timestamps_round_trips,
attempt_round_trips_with_timestamps,
merge_candidate_round_trips_with_timestamps)

### B10

WHEN Fluent reads a pre-existing Task, Attempt, or Merge Candidate
JSON file that does not contain the new timestamp fields,
THE SYSTEM SHALL deserialize cleanly with None values for the
missing fields and SHALL NOT fail the read or attempt any backfill.
Test: src/work_model.rs (legacy_json_without_timestamp_fields_deserializes_to_none)

### B11

WHEN Fluent writes a Task, Attempt, or Merge Candidate JSON file,
THE SYSTEM SHALL skip emitting timestamp keys whose value is None,
so on-disk JSON stays compact and existing JSON files remain stable
when re-saved unchanged.
Test: src/work_model.rs (task_default_serializes_without_timestamps,
legacy_merge_candidate_without_ready_at_round_trips_without_field)

### B12

WHEN a code-producing Attempt passes review and its Learner has not succeeded,
THE SYSTEM SHALL leave `attempt.completed_at` and its Merge Candidate's
`ready_at` unset.
Test: src/work_attempt_loop.rs (learner_only_resume_uses_attempt_coder_mapping)

### B13

WHEN a code-producing Attempt's Learner succeeds initially or on retry,
THE SYSTEM SHALL record one first-readiness time as both the Merge Candidate's
`ready_at` and the Attempt's `completed_at`.
Test: src/work_attempt_loop.rs (learner_runs_after_first_pass_without_findings,
learner_only_resume_uses_attempt_coder_mapping)

### B14

WHEN a legacy Merge Candidate without `ready_at` next passes the Learning
readiness gate,
THE SYSTEM SHALL stamp its first-readiness time without changing its identity.
Test: src/work_model.rs (legacy_merge_candidate_without_ready_at_round_trips_without_field,
merge_candidate_readiness_stamps_one_shared_idempotent_time)

## Per-test log output

### B1

WHEN a Rust binary test in `tests/binary.rs` runs through the
`LoggedCommand` harness wrapper,
THE SYSTEM SHALL write the test's full captured stdout and stderr to
`tests/output/<test-name>.log`, truncating any prior content from a
previous run.
Test: tests/binary.rs (log_command_writes_log_file_on_success)

### B2

WHEN a shell behavior test function in `tests/behaviors/` invokes the
shared `run_test` helper,
THE SYSTEM SHALL write the function's full captured stdout and stderr to
`tests/output/<test-file-name>/<case-name>.log`, truncating any prior
content from a previous run.
Test: tests/behaviors/lib/test-log-harness.sh

### B3

WHEN any test writes its captured output to the durable log file,
THE SYSTEM SHALL also pass the output through to the user's terminal
unchanged so interactive runs see exactly what they would see without
logging.
Test: tests/behaviors/lib/test-log-harness.sh

### B4

WHEN the test harness writes per-test log files,
THE SYSTEM SHALL create `tests/output/` and any required subdirectories
on demand and SHALL NOT fail the test on a log-write error; instead it
surfaces a one-line warning and continues.
Test: tests/lib/log.rs (write_log error handling)

### B5

WHEN `FLUENT_TESTS_SKIP_LOG=1` is set in the test process environment,
THE SYSTEM SHALL bypass per-test log-writing entirely — no file created,
no warning printed.
Test: tests/binary.rs (log_command_skips_on_fluent_tests_skip_log)
Test: tests/behaviors/lib/test-log-harness.sh

### B6

WHEN any case fails during the test run,
THE SYSTEM SHALL print a "Failing case logs:" section at the end of the
run listing each failed case's absolute log path and the last 20 lines
of that log inline.
Test: tests/behaviors/lib/test-log-harness.sh
Test: tests/binary.rs (log_command_failed_command_appends_to_failed_sentinel)

### B7

WHEN the test harness lists per-test log paths in the failed-test
summary,
THE SYSTEM SHALL print absolute paths so clipboard-copy-paste into
another shell works without `cd` context.
Test: tests/behaviors/lib/test-log-harness.sh

### B8

WHEN `tests/output/` is present in the repository,
THE SYSTEM SHALL gitignore it so per-run logs never appear in
`git status`, never get committed, and never appear in diffs.
Test: .gitignore (tests/output/ entry)

## Task dependencies

### B1

WHEN a Task has a `depends_on` field referencing another Task,
THE SYSTEM SHALL skip that Task in the ready-to-run check until the
dependency completes.
Test: src/work_model.rs (task_with_depends_on_round_trips)
Test: src/work_attempt_loop.rs (tasks_ready_to_run_skips_reviewers_until_tester_complete)
Test: src/work_attempt_loop.rs (tasks_ready_to_run_returns_dependent_after_tester_completes)

### B2

WHEN a Task has no `depends_on` field,
THE SYSTEM SHALL consider it immediately ready to run.
Test: src/work_attempt_loop.rs (tasks_ready_to_run_returns_independent_tasks_immediately)
Test: src/work_model.rs (task_without_depends_on_omits_field)

## Auto-merge watcher

### B1

WHEN `fluent auto-merge <work-item-id>` is invoked,
THE SYSTEM SHALL poll the named Work Item's state every 30 seconds and fire
`fluent merge-candidate land <work-item-id> <merge-candidate-id>` on the latest
Attempt's Merge Candidate WHEN that Attempt is `Complete` with
`review_state == passed` and Learning in the `Succeeded` state, the candidate's
`merge_state.status == pending`, and `merge_state.auto_merge_skipped` is not
`true`.
Test: src/auto_merge.rs (find_ready_candidate_ready_for_real_passed_ordinary_candidate)
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_learner_in_progress)
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_learner_handoff_pending)
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_learner_failed)
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_learning_absent)
Test: tests/binary.rs (auto_merge_exits_clean_on_sigterm)

### B2

WHEN `fluent auto-merge --all` is invoked,
THE SYSTEM SHALL poll every Work Item in the project every 30
seconds and fire merge on any Merge Candidate that satisfies the
ready conditions above. Each WI is evaluated independently each
tick.
Test: src/auto_merge.rs (find_ready_candidate_ready_for_real_passed_ordinary_candidate)

### B3

WHEN `fluent auto-merge` is invoked with both `<work-item-id>`
and `--all` set,
THE SYSTEM SHALL exit non-zero with a clear error stating the two
modes are mutually exclusive.
Test: tests/binary.rs (auto_merge_with_both_flags_set_errors)

### B4

WHEN the watcher fires a merge that succeeds,
THE SYSTEM SHALL print
`[auto-merge] merged <work-item-id> at <commit-sha>` to stderr.
Test: src/auto_merge.rs (classify_merge_outcome_succeeds_on_ok)

### B5

WHEN the watcher fires a merge that fails for a reason other than
authentication,
THE SYSTEM SHALL set `merge_state.auto_merge_skipped = true` on
the Merge Candidate, persist it via the store, print
`[auto-merge] skipping <work-item-id> (merge failed: <one-line
reason>)` to stderr, and continue polling other Work Items.
Test: src/auto_merge.rs (classify_merge_outcome_treats_other_errors_as_failed)
Test: src/work_model.rs (mark_merge_candidate_auto_merge_skipped_round_trips)

### B6

WHEN the watcher fires a merge that fails with an authentication
error (e.g., the agentic rebase agent returns 401),
THE SYSTEM SHALL print
`[auto-merge] authentication expired, pausing <work-item-id>` to
stderr, leave `auto_merge_skipped` unset, and continue polling.
Test: src/auto_merge.rs (classify_merge_outcome_recognizes_401_as_auth_expired)
Test: src/auto_merge.rs (classify_merge_outcome_recognizes_invalid_authentication_phrase)
Test: src/auto_merge.rs (classify_merge_outcome_recognizes_authentication_failed)

### B7

WHEN a future tick observes a Merge Candidate with
`merge_state.auto_merge_skipped == true`,
THE SYSTEM SHALL skip that candidate and SHALL NOT attempt to
merge it, even if the Attempt and `merge_state.status` otherwise
satisfy the ready conditions.
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_auto_merge_skipped)
Test: tests/binary.rs (auto_merge_skips_candidate_already_marked_skipped)

### B8

WHEN a future tick observes a Merge Candidate whose latest Attempt is not
`Complete`-with-passing-reviews (for example, the Attempt is paused `needs-user`),
or whose `merge_state.status` is not `pending`,
THE SYSTEM SHALL skip the candidate without setting
`auto_merge_skipped` and SHALL pick it up automatically on a
later tick if it transitions back to a ready state.
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_attempt_needs_user)
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_merge_status_needs_user)

### B9

WHEN the watcher observes a post-merge corrective Merge Candidate whose Attempt is
`Complete` with passing reviews and which is unmerged,
THE SYSTEM SHALL treat it as ready identically to an ordinary candidate, using the
same readiness rule and the same common lander.
Test: src/auto_merge.rs (find_ready_candidate_ready_for_fix_depth_corrective_candidate)

### B10

WHEN `fluent auto-merge` receives SIGINT or SIGTERM while
no merge is in progress,
THE SYSTEM SHALL exit zero immediately.
Test: tests/binary.rs (auto_merge_exits_clean_on_sigterm)

### B11

WHEN `fluent auto-merge` receives SIGINT or SIGTERM while a
merge is in progress,
THE SYSTEM SHALL allow the current merge invocation to complete
(success, failure, or hang on a long agentic rebase), persist any
resulting state changes via the store, then exit.
Untestable: Requires a long-running merge mock; verified by code inspection of the polling loop

### B12

WHEN the watcher's polling tick reads Work Item state from
`.fluent/work/`,
THE SYSTEM SHALL NOT modify any state files itself; state mutation
happens only through `fluent merge-candidate land` invocations and the
store APIs those invocations call.
Test: src/auto_merge.rs (find_ready_candidate_ready_for_real_passed_ordinary_candidate)

### B13

WHEN the watcher sees a Work Item with no Attempts or no Merge
Candidates yet,
THE SYSTEM SHALL skip it on this tick without printing anything
and re-evaluate on the next tick.
Test: src/auto_merge.rs (find_ready_candidate_returns_none_when_no_attempts)

### B14

WHEN `fluent auto-merge` is invoked with neither a Work Item
ID nor `--all`,
THE SYSTEM SHALL exit non-zero with a clear error.
Test: tests/binary.rs (auto_merge_with_neither_flag_set_errors)

### B15

WHEN `fluent auto-merge` runs and a Merge Candidate
satisfies the ready conditions on multiple consecutive ticks,
THE SYSTEM SHALL fire merge only once and then either mark the
candidate skipped (on failure) or wait for the candidate to
transition to `merged` (on success); subsequent ticks observe
the new state and do not re-fire.
Untestable: Emergent from merge success transitioning status to `merged` and merge failure setting `auto_merge_skipped`, both of which prevent re-fire on the next tick

### B16

WHEN `MergeCandidateMergeState` is deserialized from JSON that
does not contain `auto_merge_skipped`,
THE SYSTEM SHALL default the field to `None` (backward-compatible).
Test: src/work_model.rs (legacy_merge_state_json_deserializes_with_none_skipped)

### B17

WHEN `MergeCandidateMergeState` is serialized with
`auto_merge_skipped == None`,
THE SYSTEM SHALL omit the field from the JSON output.
Test: src/work_model.rs (merge_state_skips_serializing_auto_merge_skipped_when_none)

### B18

WHEN `fluent auto-merge` is invoked without `--post-merge-review`,
THE SYSTEM SHALL invoke each fresh land with post-merge review disabled.
Test: src/auto_merge.rs (auto_merge_post_merge_review_policy_defaults_off)

### B19

WHERE `--post-merge-review` is passed to `fluent auto-merge`,
THE SYSTEM SHALL invoke each fresh land with post-merge review enabled.
Test: src/auto_merge.rs (auto_merge_post_merge_review_policy_honors_opt_in)

## Autonomous Claude startup

### B1

WHEN Fluent launches an autonomous Claude worker,
THE SYSTEM SHALL pass `--safe-mode` so user, project, and local
customizations, including SessionStart and Stop hooks, do not run. The worker
retains built-in tools, authentication, selected model, and Fluent's sandbox.
Test: src/coder.rs (autonomous_claude_commands_use_safe_mode)
Test: tests/binary.rs (autonomous_claude_roles_do_not_invoke_user_hooks)

### B2

WHEN Fluent launches an intentionally interactive Claude session,
THE SYSTEM SHALL NOT pass `--safe-mode`, so user customizations remain enabled.
Test: src/coder.rs (interactive_claude_commands_preserve_user_customizations)

### B3

WHEN Fluent launches an autonomous Claude Writer with a managed `HOME` while
Fluent's sandbox is disabled, THE SYSTEM SHALL inject the supported Claude
credential from the host before launch, while leaving the interactive Claude
configuration and session state outside the managed home.
Test: src/work_task_executor.rs (only_managed_home_claude_launches_need_the_unsandboxed_credential_bridge)
Test: src/work_task_executor.rs (claude_writer_launch_uses_managed_home)
Test: src/credential.rs (claude_credential_bridge_reads_oauth_for_managed_home_launch)

### B4

WHEN an autonomous Claude launch reaches its configured deadline without model
or tool activity and Fluent confirms that the provider process has stopped,
THE SYSTEM SHALL retry the same Task within the configured recovery budget
without consuming a Writer round.
Test: src/coder.rs (safe_claude_no_progress_launch_is_terminated_and_typed)
Test: src/coder.rs (claude_no_progress_retries_same_logical_run_and_preserves_phases)
Test: tests/binary.rs (scheduler_recovers_a_safe_claude_no_progress_launch_inside_the_same_task)

### B5

IF an autonomous Claude launch has model or tool activity, or Fluent cannot
prove that no such activity occurred,
THEN THE SYSTEM SHALL continue that launch without applying no-progress
recovery.
Test: src/transcript_pump.rs (claude_startup_retry_records_remain_pre_progress)
Test: src/transcript_pump.rs (claude_assistant_record_marks_progress)
Test: src/transcript_pump.rs (claude_progress_classifier_fails_closed_on_malformed_or_unknown_records)
Test: src/coder.rs (claude_assistant_progress_disables_no_progress_termination)
Test: src/coder.rs (partial_claude_record_at_deadline_disables_no_progress_termination)

### B6

IF the configured Claude no-progress retry budget exhausts before model
progress,
THEN THE SYSTEM SHALL pause the same Task as `provider-unavailable` with guidance
to resume the same Attempt.
Test: src/coder.rs (claude_no_progress_exhaustion_remains_typed)
Test: src/work_task_executor.rs (terminal_coder_failures_bypass_generic_retries)
Test: tests/binary.rs (scheduler_pauses_the_same_task_after_claude_no_progress_exhaustion)

## Claude readiness diagnosis

### B1

WHEN configured initialization checks Claude readiness,
THE SYSTEM SHALL check the local command and authentication status before
changing the project.
Test: tests/binary.rs (mixed_coder_profile_preflights_both_providers)

### B1a

WHEN a configured initialization selects Claude,
THE SYSTEM SHALL require one bounded, tool-free successful response from each
distinct selected Claude model before changing the project.
Test: tests/binary.rs (configured_claude_readiness_requires_live_response_before_mutation)
Test: tests/binary.rs (configured_claude_readiness_uses_bounded_tool_free_canary)
Test: tests/binary.rs (configured_claude_readiness_checks_each_distinct_model)
Test: tests/binary.rs (configured_claude_readiness_timeout_preserves_uninitialized_project)

### B1b

WHEN configured init checks Claude readiness on macOS while the host Claude Code
login is valid,
THE SYSTEM SHALL recognize that login without exposing the credential in
diagnostics even when the isolated worker home has no Claude login state.
Test: tests/binary.rs (configured_claude_readiness_bridges_keychain_before_auth_probe)

### B2

IF any configured provider fails readiness during initialization,
THEN THE SYSTEM SHALL preserve the uninitialized project unchanged.
Test: tests/binary.rs (coder_profile_preflight_failure_preserves_uninitialized_project)

### B3

IF a Claude readiness or credential-refresh command fails,
THEN THE SYSTEM SHALL report a stable category code that distinguishes
authentication, host credential boundary, rate limit, network, provider
availability, timeout, and unknown failures.
Test: src/claude_diagnostics.rs (classifies_auth_rate_network_provider_and_host_failures)
Test: src/claude_diagnostics.rs (category_codes_are_stable)
Test: tests/binary.rs (coder_profile_preflight_keeps_network_failure_out_of_login_recovery)

### B4

WHEN Fluent reports a Claude command diagnostic,
THE SYSTEM SHALL redact structured and free-text credential values before
storing or printing it.
Test: src/claude_diagnostics.rs (diagnostic_redacts_structured_and_free_text_secrets)
Test: tests/binary.rs (coder_profile_preflight_redacts_failed_claude_diagnostics)

### B5

WHEN Fluent stores or prints a Claude command diagnostic,
THE SYSTEM SHALL limit it to 512 characters, including its truncation marker.
Test: src/claude_diagnostics.rs (diagnostic_is_bounded_on_unicode_boundaries)

### B6

WHEN `claude auth status --json` returns valid JSON reporting that no user is
logged in, regardless of the command's exit status,
THE SYSTEM SHALL prescribe `claude /login` only when the available evidence
indicates authentication.
Test: src/claude_diagnostics.rs (authentication_is_the_only_recovery_that_names_login)
Test: tests/binary.rs (coder_profile_preflight_treats_nonzero_logged_out_status_as_authentication)

### B7

IF Claude cannot report an active login because the enclosing host blocked
credential access,
THEN THE SYSTEM SHALL prescribe the scoped host-agent approval flow and SHALL
NOT prescribe login.
Test: tests/binary.rs (coder_profile_preflight_treats_nonzero_logged_out_status_as_blocked_keychain)

### B8

WHEN Claude authentication readiness succeeds and `claude --version` reports a
version older than 2.1.211,
THE SYSTEM SHALL warn that the installed version predates the parallel session
token-renewal fix and SHALL continue initialization.
Test: src/provider_readiness.rs (old_claude_version_warns_but_current_version_does_not)
Test: tests/binary.rs (coder_profile_preflight_warns_for_old_claude_parallel_auth)

### B9

IF Claude authentication readiness succeeds but Fluent cannot parse the
installed Claude Code version,
THEN THE SYSTEM SHALL continue without a version warning.
Test: src/provider_readiness.rs (unknown_claude_version_does_not_block_readiness)

## Claude auth token expiry detection

### B1

WHEN any Claude coder variant (`SandboxedClaudeCode`,
`BareClaudeCode`) is about to launch the `claude` process,
THE SYSTEM SHALL call `ensure_not_expired_with_refresh()` first,
which checks `claude_auth::ensure_not_expired()` and, on failure,
attempts `credential::refresh_credentials()` once and rechecks.
The Task also fails if the refresh probe times out or exits unsuccessfully.
Untestable: Structural integration verified by code inspection — `ensure_not_expired_with_refresh()` is the first call in both `SandboxedClaudeCode::run` and `BareClaudeCode::run`

### B2

WHEN `claude_auth::ensure_not_expired()` is invoked,
THE SYSTEM SHALL read the keychain entry under service
`Claude Code-credentials`, parse the `claudeAiOauth` object, and
return `Ok(())` if the access token's `expiresAt` is more than 5
minutes in the future.
Test: src/claude_auth.rs (tests::check_token_expiry_returns_ok_when_more_than_5min_remaining)
Test: src/claude_auth.rs (tests::keychain_envelope_deserializes_with_refresh_token)

### B3

WHEN `claude_auth::ensure_not_expired()` finds the access token
within 5 minutes of `expiresAt` (or already expired),
THE SYSTEM SHALL return `AuthError::Expired { expires_at }` with
the parsed expiry, so callers can produce a user-facing message
naming the recovery action (`claude /login`).
Test: src/claude_auth.rs (tests::check_token_expiry_returns_expired_within_margin)
Test: src/claude_auth.rs (tests::check_token_expiry_returns_expired_when_already_expired)
Test: src/claude_auth.rs (tests::check_token_expiry_boundary_at_exactly_5min)

### B4

WHEN `claude_auth::ensure_not_expired()` cannot read the keychain
entry (missing entry, malformed JSON, decode error),
THE SYSTEM SHALL return `Ok(())` rather than an error, on the
assumption that the user is on an API-key path that doesn't use
the keychain. The coder runs as today; any genuine auth failure
surfaces through the recovery layer.
Test: src/claude_auth.rs (tests::check_token_expiry_returns_ok_when_no_creds)
Test: src/claude_auth.rs (tests::keychain_envelope_deserializes_without_claude_ai_oauth)

### B5

WHEN the keychain entry's `claudeAiOauth.refreshToken` field is
absent or null,
THE SYSTEM SHALL return `Ok(())` (treating the session as API-key
only) and skip the expiry check.
Test: src/claude_auth.rs (tests::check_token_expiry_returns_ok_when_no_refresh_token)
Test: src/claude_auth.rs (tests::keychain_envelope_deserializes_without_refresh_token)

### B6

WHEN `src/coder.rs::run_with_transcript_retrying` observes the
coder process exit non-zero AND the transcript reports either a most-recent
`result` event with `api_error_status == 401` or an `assistant` event with
`error == "authentication_failed"`,
THE SYSTEM SHALL attempt `credential::refresh_credentials()` once
and re-run the coder session only after the refresh probe succeeds and Fluent
rereads the credential store. If the probe times out or exits unsuccessfully,
THE SYSTEM SHALL return `AuthError::RefreshFailed` with classified diagnostic
evidence so the existing Attempt pauses for the matching recovery. If the
re-run still fails authentication,
THE SYSTEM SHALL return `AuthError::Rejected { request_id }` for a 401 or
`AuthError::NotLoggedIn` for `authentication_failed`, so the caller pauses with
a recovery message. The rejected variant copies `result.request_id` when present.
Test: src/claude_auth.rs (tests::classify_transcript_401_returns_rejected_on_result_401)
Test: src/claude_auth.rs (tests::classify_transcript_401_extracts_request_id_when_present)
Test: src/claude_auth.rs (tests::classify_transcript_401_returns_rejected_with_none_request_id_when_missing)
Test: src/claude_auth.rs (tests::classify_transcript_auth_failure_recognizes_not_logged_in)
Test: src/coder.rs (coder_retries_once_after_credential_refresh_on_401)
Test: src/coder.rs (coder_retries_once_then_surfaces_structured_login_failure)
Test: src/coder.rs (coder_surfaces_auth_error_when_refresh_does_not_help)
Test: src/coder.rs (failed_refresh_probe_stops_the_auth_retry)

### B7

WHEN the recovery layer's structured authentication detection fires alongside the
existing rate-limit detection on the same attempt,
THE SYSTEM SHALL prefer the authentication surface (the auth issue is the
proximate cause; the rate-limit envelope may be incidental).
Untestable: Structural ordering verified by code inspection — `classify_transcript_auth_failure` is called before `parse_rate_limit_from_transcript` in `run_with_transcript_retrying`

### B8

WHEN the user-facing error message is constructed for `AuthError::Expired`,
`AuthError::Rejected`, or `AuthError::NotLoggedIn`,
THE SYSTEM SHALL name `claude /login` and `fluent attempt run`. WHEN it is
constructed for `AuthError::RefreshFailed`, THE SYSTEM SHALL name
`claude /login` only for authentication evidence and SHALL name the matching
non-authentication repair plus `fluent attempt run` otherwise.
Test: src/claude_auth.rs (tests::auth_error_expired_user_message_names_login_action)
Test: src/claude_auth.rs (tests::auth_error_rejected_user_message_names_login_action)
Test: src/claude_auth.rs (tests::auth_handoff_names_attempt_run_recovery)
Test: src/claude_auth.rs (tests::non_auth_refresh_failure_does_not_name_login)

### B9

WHEN any Codex coder variant (`CodexCode`) is about to launch,
THE SYSTEM SHALL NOT call `claude_auth::ensure_not_expired()`.
Codex auth lifecycle is out of scope for this Work Item.
Untestable: Structural absence verified by code inspection — `CodexCode::run` does not reference `claude_auth`

### B10

IF a Claude Task exits with structured `authentication_failed` evidence,
THEN THE SYSTEM SHALL classify it as an authentication pause and SHALL NOT
consume the generic coder retry budget.
Test: src/work_task_executor.rs (structured_claude_login_failure_is_an_auth_pause_without_retry)

## Credential refresh and retry

### B1

WHEN a coder session fails with an auth 401, or the pre-launch
expiry check finds the token expired,
THE SYSTEM SHALL attempt `credential::refresh_credentials()` once
and re-run the coder session only after the probe succeeds and Fluent rereads
credentials. If the probe times out or exits unsuccessfully, THE SYSTEM SHALL
pause the existing Attempt for the classified recovery before a Writer round is
consumed.
Test: src/coder.rs (coder_retries_once_after_credential_refresh_on_401)

### B2

WHEN Fluent starts the Claude refresh probe,
THE SYSTEM SHALL run it in safe mode and finish it within
`FLUENT_CLAUDE_REFRESH_DEADLINE_SECS` (default 30 seconds).
Test: src/credential.rs (refresh_probe_honors_configured_deadline)

### B3

IF the refresh probe exceeds its deadline,
THEN THE SYSTEM SHALL attempt to terminate and reap its owned process group. If
Fluent confirms the sweep, it SHALL return a typed timeout failure that
preserves the same Attempt for recovery; otherwise it SHALL return
a typed cleanup failure that retains the unconfirmed-sweep diagnostic.
Test: src/credential.rs (refresh_probe_timeout_terminates_process_tree)
Test: tests/binary.rs (refresh_timeout_preserves_provider_paused_attempt)

### B4

IF the refresh probe exits unsuccessfully,
THEN THE SYSTEM SHALL capture bounded output without blocking on a full pipe,
redact credential values, report the classified refresh failure, and SHALL NOT
reread credentials as though refresh had succeeded.
Test: src/credential.rs (refresh_probe_nonzero_exit_is_failure)
Test: src/credential.rs (refresh_probe_retains_bounded_sanitized_diagnostic)

### B5

WHERE the refresh probe succeeds before its deadline,
THE SYSTEM SHALL reread credentials and continue the existing single-retry flow.
Test: src/credential.rs (successful_refresh_probe_rereads_credentials)

### B6

WHERE the refresh resolves the credential (the re-run session no
longer returns a 401),
THE SYSTEM SHALL continue the task normally, with no pause.
Test: src/coder.rs (coder_succeeds_on_retry_when_refresh_fixes_auth)

### B7

WHERE the session still returns a 401 after the refresh and retry,
THE SYSTEM SHALL fail the task and pause the Attempt to `needs-user`
marked with the `auth` pause kind.
Test: src/coder.rs (coder_surfaces_auth_error_when_refresh_does_not_help)

### B8

WHEN refresh-on-401-retry recovers a session (B5/B6),
THE SYSTEM SHALL post a local notification that it recovered after
a credential refresh, mirroring the rate-limit resume notification.
Test: src/coder.rs (recovered_after_refresh_posts_notification)

## Resumable needs-user attempts

### B1

WHEN an Attempt pauses to `needs-user`,
THE SYSTEM SHALL record the pause kind on the Attempt (`auth`,
`uncertain`, or `round-cap`) and keep the Attempt in a suspended,
resumable state rather than a terminal one.
Test: src/work_attempt_loop.rs (auth_pause_records_kind_and_leaves_attempt_resumable)
Test: src/work_attempt_loop.rs (budget_exhaustion_records_round_cap_pause_kind)
Test: src/work_attempt_loop.rs (uncertain_reviews_record_uncertain_pause_kind)

### B2

WHEN `fluent attempt run` is invoked on an Attempt suspended with
the `auth` pause kind,
THE SYSTEM SHALL re-run only the tasks that failed on auth, against
the existing writer commit, without starting a fresh Attempt or
discarding committed work.
Test: src/work_attempt_loop.rs (resume_auth_pause_reruns_only_auth_failed_tasks)

### B3

WHEN the re-run auth-failed tasks pass,
THE SYSTEM SHALL continue the Attempt through its normal transitions
(to a Merge Candidate, or the next write round).
Test: src/work_attempt_loop.rs (resume_auth_pause_advances_to_merge_candidate_on_pass)

### B4

WHEN an Attempt suspends with the `auth` pause kind,
THE SYSTEM SHALL post a local notification prompting the user to
re-authenticate (`claude /login`) and then run `fluent attempt run`.
Test: src/work_task_executor.rs (auth_suspend_posts_reauth_notification)

### B5

WHEN an Attempt is suspended with the `auth` pause kind,
THE SYSTEM SHALL write a handoff that names the recovery that
exists: re-authenticate (`claude /login`), then `fluent attempt run`.
Test: src/claude_auth.rs (auth_handoff_names_attempt_run_recovery)

### B6

WHEN `fluent attempt run` is invoked on an Attempt suspended with a
pause kind whose resume is not implemented (for example, a `round-cap`
Attempt without an approved extension, legacy provider migration, or validated
usage-limit Writer recovery approval),
THE SYSTEM SHALL report that the pause needs its specific input and
leave the Attempt suspended.
Test: src/work_attempt_loop.rs (resume_unimplemented_kind_reports_clearly_and_leaves_suspended)

WHEN `fluent attempt run` or `fluent queue resume` revisits a verified
assignment-mismatch pause, THE SYSTEM SHALL reevaluate the saved assignment in
the same Attempt.
Test: tests/binary.rs (saved_writer_assignment_recovers_through_public_resume)

WHEN assignment-mismatch recovery is admitted, THE SYSTEM SHALL preserve the
completed Writer, source evidence, corrected candidate, completion matrix, and
Task history.
Test: tests/binary.rs (saved_writer_assignment_recovery_preserves_history)

WHEN the same eligible queue resume repeats, THE SYSTEM SHALL reuse the saved
assignment, existing Tasks, and active dispatch without launching the completed
Writer again.
Test: tests/binary.rs (saved_writer_assignment_resume_is_idempotent)

IF the saved assignment, corrected candidate, clean workspace, evidence, or
execution ownership cannot be verified, THEN THE SYSTEM SHALL leave the pause
unchanged and report the specific missing or conflicting binding.
Test: tests/binary.rs (saved_writer_assignment_resume_rejects_unsafe_state)
Test: tests/binary.rs (corrective_writer_assignment_rejects_invalid_authority)

### B6a

WHEN `fluent attempt run` resumes an Attempt paused by uncertain reviewer
verdicts, and every Task is complete, the latest reviewer evidence matches the
unchanged clean Writer candidate, and no latest verdict is `fail`,
THE SYSTEM SHALL add a new review round containing only the reviewer roles whose
latest verdict is uncertain, preserve completed passing reviews, clear the
pause, and continue the same Attempt.
Test: src/work_model.rs (completed_uncertain_review_pause_is_resumable_but_active_work_is_not)
Test: src/work_attempt_loop.rs (uncertain_review_resume_reopens_only_uncertain_roles)
Test: src/work_attempt_loop.rs (retried_uncertain_role_combines_with_preserved_passes)

### B6b

IF the candidate HEAD changed, the candidate is dirty, reviewer evidence is
missing or does not match the Writer candidate, any latest verdict is `fail`, or
a Task is incomplete,
THEN THE SYSTEM SHALL reject uncertain-review recovery without changing the
Work Item.
Test: src/work_attempt_loop.rs (uncertain_review_resume_rejects_missing_evidence_without_mutation)
Test: src/work_attempt_loop.rs (uncertain_review_resume_rejects_dirty_candidate_without_mutation)
Test: src/work_attempt_loop.rs (uncertain_review_resume_rejects_moved_candidate_without_mutation)
Test: src/work_attempt_loop.rs (uncertain_review_resume_rejects_stale_review_context_without_mutation)
Test: src/work_attempt_loop.rs (uncertain_review_resume_rejects_a_latest_failed_verdict_without_mutation)
Test: src/work_model.rs (completed_uncertain_review_pause_is_resumable_but_active_work_is_not)

### B6c

WHEN `fluent queue resume <work-item-id> --attempt <attempt-id>` names an
uncertain-review pause that satisfies B6a,
THE SYSTEM SHALL apply the same recovery checks before queueing a resume bound
to that exact Attempt.
Test: src/queue.rs (resume_queues_a_valid_uncertain_review_recovery)
Test: tests/binary.rs (queue_resume_accepts_verified_uncertain_review_pause)

## Usage-limit Writer recovery approval

### B1

WHEN an operator requests recovery and Fluent verifies the usage-limit failure,
assignment, unchanged partial work, and settled ownership,
THE SYSTEM SHALL record approval bound to that Writer, evidence, candidate
commit, and uncommitted changes.
Test: tests/binary.rs (usage_limit_writer_recovery_records_exact_approval)
Example: The saved failed Writer has both a committed review correction and an
unfinished second correction. Neither form of progress makes it ineligible
solely because model activity occurred before the usage-limit failure.

### B2

WHEN recovery approval is recorded,
THE SYSTEM SHALL keep the Attempt paused without launching or queueing workers
until a separate explicit resume.
Test: tests/binary.rs (usage_limit_writer_recovery_does_not_launch_workers)
Example: Recording approval does not start the Writer. Returning an existing
approval later does not reapply this initial pause to a retry already running
or finished.

### B3

IF required proof is missing or contradictory, partial work has changed, or
ownership remains unsettled,
THEN THE SYSTEM SHALL reject recovery without changing the Work records or
candidate.
Test: tests/binary.rs (usage_limit_writer_recovery_rejects_unproven_state)
Example: A usage-limit phrase in model-authored text alone does not prove a
provider interruption; an unrelated failure remains outside this recovery.

## Usage-limit Writer resume

### B1

WHEN an operator explicitly resumes a still-valid approved recovery,
THE SYSTEM SHALL retry the same Writer Task in the same Attempt from its
preserved partial work, using its saved coder settings without consuming another
Writer round.
Test: tests/binary.rs (usage_limit_writer_resume_retries_same_task)
Example: Saved coder settings include the provider, model, and effort. Recovery
does not reset, stash away, commit, or rewrite the partial work to make it
eligible; the resumed Writer can continue its authorized implementation work.
A validated usage-limit recovery approval supplies the specific input required
by Resumable needs-user attempts:B6. Without that approval, the refusal remains
applicable.

### B2

WHEN the retry begins,
THE SYSTEM SHALL retain the original correction assignment and prior failure
and review artifacts unchanged.
Test: tests/binary.rs (usage_limit_writer_resume_preserves_assignment_and_history)
Example: The new execution can produce new evidence, but it cannot overwrite
the preserved failed execution or replace its assigned review findings.

### B3

IF the approved evidence, assignment, candidate, or uncommitted changes no longer
match, or execution ownership is live or unverifiable,
THEN THE SYSTEM SHALL reject resume before launching a worker, without changing
Work records or the candidate.
Test: tests/binary.rs (usage_limit_writer_resume_rejects_stale_or_live_state)

## Usage-limit Writer preflight

A prepared retry is the same approved Writer after preparation and before its
first launch. A preflight-only pause is a host readiness failure before
reservation, with native evidence that distinguishes it from a later stop or
Worker failure.

### B1

WHEN an approved Writer retry checks Codex readiness with its private home in
the host temporary tree and its workspace outside that tree,
THE SYSTEM SHALL permit the production readiness check inside its restricted
Worker sandbox.
Test: tests/binary.rs (usage_limit_writer_recovery_real_confinement)
Derived: Usage-limit Writer resume:B1 requires this first-launch boundary to work.
Example: Successful shell launch alone does not prove that Codex can resolve its
private home and report authentication status from the selected working directory.

### B2

WHILE the readiness check accesses its private home,
THE SYSTEM SHALL deny access to unrelated temporary and operator-home content
outside the explicitly admitted roots.
Test: tests/binary.rs (usage_limit_writer_recovery_real_confinement)
Derived: Preserve Usage-limit Writer tool access:B2 and the existing restricted
retry's read boundaries; ancestor metadata does not authorize sibling contents.

### B3

WHEN an operator explicitly resumes a still-valid prepared retry after a
provable preflight-only host-sandbox pause,
THE SYSTEM SHALL retry the same Writer Task under its existing approval without
consuming another Writer round.
Test: tests/binary.rs (usage_limit_writer_preflight_pause_resumes_same_task)
Modifies: Usage-limit Writer resume:B1 — explicitly include the native
pre-reservation host-sandbox pause before the first approved launch.

### B4

IF the preflight-only pause cannot be proved, a later stop or terminal failure
occurs, the approved inputs change, or ownership is live or unverifiable,
THEN THE SYSTEM SHALL reject the earlier approval as restart authority before
launch or Work mutation.
Test: tests/binary.rs (usage_limit_writer_preflight_pause_rejects_unproven_state)
Derived: Preserve Usage-limit Writer resume:B3 and Usage-limit recovery repeats
and later interruptions:B3 across the newly admitted state.

### B5

WHEN public foreground or queued recovery resumes a valid preflight-only pause,
THE SYSTEM SHALL preserve the original assignment and historical evidence
through the single approved launch.
Test: tests/binary.rs (usage_limit_writer_preflight_pause_queue_preserves_history)
Derived: Usage-limit Writer resume:B2 and Usage-limit recovery repeats and later
interruptions:B2 apply to this recovery state too.

### B6

WHEN status or Work show observes a prepared retry paused before launch,
THE SYSTEM SHALL report a current eligible public resume command only while
the same recovery checks pass.
Test: tests/binary.rs (usage_limit_writer_preflight_pause_guidance_revalidates)
Derived: Existing candidate-bound pause guidance must cover this admitted state
without changing Work during observation.

## Usage-limit Writer tool access

### B1

WHEN an approved usage-limit Writer retry performs focused Rust verification,
THE SYSTEM SHALL permit the installed toolchain to build and execute the
verification from the Writer's workspace using its private dependency cache.
Test: tests/binary.rs (usage_limit_writer_recovery_real_confinement)
Derived: Usage-limit Writer resume:B1 requires the same Writer to continue its
authorized implementation work; Writer dependency-cache confinement:B6 already
permits offline focused verification with admitted dependencies.
Example: An offline minimal Rust test must compile and run through the actual
retry profile. Starting a shell or printing a tool version is insufficient proof.

### B2

WHILE an approved usage-limit Writer retry uses installed tools,
THE SYSTEM SHALL deny writes to the installed toolchain, operator dependency
cache, retained failure and review artifacts, and host-owned recovery audit.
Test: tests/binary.rs (usage_limit_writer_recovery_real_confinement)
Derived: Tool access must preserve the existing recovery confinement and
historical-evidence boundaries; it grants no new write authority outside the
Writer's existing workspace and private mutable roots.

## Usage-limit recovery repeats and later interruptions

### B1

WHEN a recovery request repeats, including after interrupted recording, and its
validated inputs still match,
THE SYSTEM SHALL complete or reuse the same approval without duplication.
Test: tests/binary.rs (usage_limit_writer_recovery_replay_is_idempotent)
Derived: Approval is bound to one specific interruption; retrying the operation
that records it does not authorize another interruption or duplicate its record.
Example: An interrupted approval can finish recording after revalidation. An
already recorded approval is returned unchanged rather than recorded again.

### B2

IF the approved Writer retry is already running or finished,
THEN THE SYSTEM SHALL NOT launch a second Writer retry under that approval.
Test: tests/binary.rs (usage_limit_writer_recovery_does_not_duplicate_retry)
Derived: The approval authorizes one recovery retry, not repeated executions.
Example: Ordinary continuation to outstanding downstream stages after Writer
completion does not count as launching a second Writer retry.

### B3

IF a later cancellation or new terminal failure occurs,
THEN THE SYSTEM SHALL reject the earlier approval as authority for another
restart.
Test: tests/binary.rs (usage_limit_writer_recovery_rejects_approval_after_new_stop_or_failure)
Derived: Authorization for the earlier interruption cannot override a later stop
or authorize recovery from a different failure.
Example: A new usage-limit interruption needs its own validated approval;
cancellation and other pause kinds retain their applicable explicit recovery.

### Retained gates

This recovery requires a dedicated approval. It does not broaden the transcript
grammar or eligibility of Legacy provider-pause recovery:B1-B3, and it does not
make unrelated failed Writers automatically retryable.

The existing Pre-review completion gate, Host-action ownership, Complete ordinary
review coverage, Correction-aware review invalidation, canonical final Tester,
and Learner readiness requirements remain applicable. Recovery approval supplies
no completion or verification
credit. Historical passing checks cannot substitute for checks required against
the resumed Writer's resulting candidate. The retained unexplained process-leak
report is not waived.

Worker confinement and the Attempt's approved round budget remain unchanged.
The existing refusal of implicit failed-Writer retries still applies outside
this explicitly approved exception.

## Legacy provider-pause recovery

### B1

WHEN `fluent attempt run` targets either preserved scheduler Attempt and every
failed Review Task contains the accepted 13-event legacy Claude outage
transcript with no reviewer verdict,
THE SYSTEM SHALL reopen those exact Review Tasks as `provider-unavailable` and
resume the same Attempt while preserving completed Tasks, candidate work, and
accumulated artifacts.
Test: src/work_attempt_loop.rs (preserved_scheduler_attempt_transcripts_resume_through_public_route)

### B2

IF any blocking Task is not a Review Task, has a reviewer verdict, or has legacy
transcript evidence that differs in event count or order, retry sequence,
session identity, terminal semantics, or zero-model-and-tool evidence,
THEN THE SYSTEM SHALL leave the Attempt at `round-cap` and explain why its
provider evidence is inconclusive.
Test: src/work_attempt_loop.rs (legacy_scheduler_transcript_deviations_remain_round_cap)

### B3

WHEN a current coder Task produces only the accepted legacy Claude outage
transcript without current numbered provider evidence,
THE SYSTEM SHALL NOT classify that Task as `provider-unavailable`.
Test: src/work_task_executor.rs (legacy_scheduler_transcript_does_not_pause_current_task)

## Keep-awake toggle

### B1

WHEN `fluent keep-awake on` is invoked on macOS,
THE SYSTEM SHALL ensure a single `caffeinate -i` process managed by
a Fluent wrapper script is running, and print
`keep-awake on (caffeinate PID <pid>)` to stdout.
Test: tests/behaviors/operations/test-keep-awake.sh (on first invocation installs LaunchAgent and wrapper script)

### B2

WHEN `fluent keep-awake on` is invoked and a Fluent-managed
caffeinate process is already running,
THE SYSTEM SHALL print `keep-awake already on (caffeinate PID
<pid>)` to stdout and exit zero without spawning a duplicate.
Test: tests/behaviors/operations/test-keep-awake.sh (on when already running prints already-on with PID)

### B3

WHEN `fluent keep-awake on` is invoked for the first time (no
LaunchAgent plist installed),
THE SYSTEM SHALL write the wrapper script to
`~/.config/fluent/keep-awake-caffeinate`, write
`~/Library/LaunchAgents/com.fluent.keep-awake.plist`, invoke
`launchctl bootstrap gui/$UID <plist>`, and print one additional
line confirming the LaunchAgent installation.
Test: tests/behaviors/operations/test-keep-awake.sh (on first invocation installs LaunchAgent and wrapper script)

### B4

WHEN `fluent keep-awake on` succeeds,
THE SYSTEM SHALL enable the LaunchAgent's RunAtLoad and KeepAlive
policies so the user's intended-on state persists across reboots.
Test: src/keep_awake.rs (plist_round_trips_keepalive_true)

### B5

WHEN `fluent keep-awake off` is invoked while a Fluent-managed
caffeinate process is running,
THE SYSTEM SHALL invoke `launchctl bootout` to unload the
LaunchAgent, rewrite the plist with KeepAlive and RunAtLoad
disabled, wait for the process to exit, and print
`keep-awake off` to stdout.
Test: tests/behaviors/operations/test-keep-awake.sh (off when running calls bootout and updates plist)

### B6

WHEN `fluent keep-awake off` is invoked and no caffeinate process
is running,
THE SYSTEM SHALL print `keep-awake already off` to stdout, and
exit zero.
Test: tests/behaviors/operations/test-keep-awake.sh (off when not running prints already-off)

### B7

WHEN `fluent keep-awake status` is invoked,
THE SYSTEM SHALL print `on (caffeinate PID <pid>)` if a
Fluent-managed caffeinate wrapper process is discoverable via
`pgrep -f <sentinel>`, or `off` otherwise, to stdout.
Test: tests/behaviors/operations/test-keep-awake.sh (status reports off when no caffeinate process is running)
Test: tests/behaviors/operations/test-keep-awake.sh (status reports on with caffeinate PID when process is running)

### B8

WHEN the macOS user logs in or the laptop boots,
THE SYSTEM SHALL respect the last toggle state: if `off` was the
most recent state, the LaunchAgent's disabled RunAtLoad and
KeepAlive policies keep caffeinate from running; if `on` was the
most recent state, launchctl starts caffeinate via the LaunchAgent.
Untestable: Requires macOS login/reboot cycle

### B9

WHEN `fluent keep-awake uninstall` is invoked,
THE SYSTEM SHALL send `SIGTERM` to the running caffeinate process
if any, invoke `launchctl bootout` against the LaunchAgent,
delete the plist file and wrapper script, and print a one-line
confirmation.
Test: tests/behaviors/operations/test-keep-awake.sh (uninstall removes plist and wrapper script)
Test: tests/behaviors/operations/test-keep-awake.sh (uninstall when already uninstalled prints already-uninstalled)

### B10

WHEN any `fluent keep-awake` subcommand is invoked on a non-macOS
platform,
THE SYSTEM SHALL exit non-zero with a clear error stating the
subcommand is macOS-only, and SHALL NOT touch the LaunchAgent or
any other state.
Test: src/keep_awake.rs (ensure_macos_errors_on_non_macos)

### B11

WHEN the LaunchAgent spawns caffeinate at boot or login,
THE SYSTEM SHALL use the Fluent wrapper script at
`~/.config/fluent/keep-awake-caffeinate` as the ProgramArguments
target so subsequent `status`, `off`, and `on` invocations
correctly observe the running process via `pgrep -f` against the
wrapper path.
Test: src/keep_awake.rs (plist_contains_valid_xml_structure)

---

## Git lock-error retry

### B1

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

### B2

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

### B3

WHEN a retried git invocation succeeds within the 8-attempt budget,
THE SYSTEM SHALL return `Ok(())` / `Ok(stdout)` / `Ok(Output)` to
the caller as if the invocation had succeeded on the first try,
with no log output and no indication that retries occurred.
Test: tests/binary.rs (git_wrapper_succeeds_after_config_lock_clears_within_budget)
Test: tests/binary.rs (git_wrapper_succeeds_after_index_lock_clears_within_budget)

### B4

WHEN the 8-attempt retry budget is exhausted,
THE SYSTEM SHALL emit one stderr line naming the failed command,
the number of attempts made, and the underlying lock-error
stderr, then return the same error type the wrapper produces for
any other non-zero exit.
Test: tests/binary.rs (git_wrapper_bails_when_lock_persists_past_budget)

### B5

WHEN the git invocation exits non-zero with stderr that does NOT
match a lock-error pattern,
THE SYSTEM SHALL return the error immediately without retrying.
Test: tests/binary.rs (git_wrapper_does_not_retry_on_non_lock_error)
Test: src/git.rs (is_lock_error_does_not_match_authentication_failure)
Test: src/git.rs (is_lock_error_does_not_match_network_error)
Test: src/git.rs (is_lock_error_does_not_match_unrelated_file_exists_error)

### B6

WHEN the git invocation exits zero on the first attempt,
THE SYSTEM SHALL return success without any sleep or retry
overhead.
Test: tests/binary.rs (git_wrapper_succeeds_on_first_attempt_when_no_lock_error)

### B7

WHEN the wrapper's backoff sleep would push wall-clock past the
total budget (~1.5s),
THE SYSTEM SHALL apply the final sleep anyway and then make the
last attempt before giving up — the budget is approximate, not
strict.
Test: tests/binary.rs (git_wrapper_bails_when_lock_persists_past_budget)
Test: src/git.rs (backoff_duration_caps_at_320ms_after_5th_attempt)

### B8

WHEN multiple concurrent Fluent processes each retry the same
git lock,
THE SYSTEM SHALL apply per-process jitter so the retries do not
collide on identical sleep intervals (thundering-herd avoidance).
Test: src/git.rs (lock_jitter_factor_within_range)
Test: src/git.rs (backoff_duration_applies_jitter_within_25_percent)

---

## Notification dispatch

### B1

WHEN `fluent::notify::notify(title, body)` is invoked,
THE SYSTEM SHALL write one line `[{title}] {body}` to stderr and
return without launching any subprocess, regardless of host platform.
Test: src/notify.rs (notify_does_not_panic)
Test: src/notify.rs (notify_format_contract)

### B2

WHEN this system is built,
THE SYSTEM SHALL contain no invocations of `osascript` inside
`src/notify.rs` (the macOS-specific delivery path is removed).
Test: tests/binary.rs (no_osascript_in_notify)

### B3

WHEN the rate-limit retry loop in `src/coder.rs` transitions between
paused and resumed states,
THE SYSTEM SHALL continue to call `notify()`, which produces stderr
output. No call-site change is required.
Test: src/coder.rs (rate_limit_state_tests::normal_to_rate_limited_fires_enter_notification)
Test: src/coder.rs (rate_limit_state_tests::full_cycle_fires_enter_once_and_leave_once)

### B4

WHEN a future general notification system (Discord, Slack, push, etc.)
replaces `notify()`'s implementation,
THE SYSTEM SHALL be able to re-light all existing call sites without
modifying them, because the `notify(title, body)` signature is
preserved.
Untestable: design-intent constraint verified by code review; the
guarantee is structural (public signature preservation), not
observable at runtime.

---

## Easy-to-answer skill rule

### B1

WHEN `skills/fluent.full/references/capture-brief.md` is read after this Work Item
lands,
THE SYSTEM SHALL contain in its "Rules" section a short rule
naming the two acceptable question patterns (labeled multi-option
where the user types a single label, and yes/no with an implicit
default) and naming the bad pattern (unlabeled "X or Y" requiring
the user to re-describe an option).
Test: tests/behaviors/operations/test-easy-to-answer-skill-rule.sh (rule present in capture-brief)

### B2

WHEN `skills/fluent.full/references/define-behaviors.md` is read after this Work
Item lands,
THE SYSTEM SHALL contain the same rule, with the same wording, in
its "Rules" section (or equivalent).
Test: tests/behaviors/operations/test-easy-to-answer-skill-rule.sh (rule present in define-behaviors)

### B3

WHEN `skills/fluent.full/references/design-approach.md` is read after this Work
Item lands,
THE SYSTEM SHALL contain the same rule, with the same wording, in
its "Rules" section (or equivalent).
Test: tests/behaviors/operations/test-easy-to-answer-skill-rule.sh (rule present in design-approach)

### B4

WHEN `skills/fluent.full/references/plan-execution.md` is read after this Work
Item lands,
THE SYSTEM SHALL contain the same rule, with the same wording, in
its "Rules" section (or equivalent).
Test: tests/behaviors/operations/test-easy-to-answer-skill-rule.sh (rule present in plan-execution)

### B5

WHEN the rule is added to any skill,
THE SYSTEM SHALL NOT add a new section above or below the
existing "Rules" section; the rule lives within that section as
one bullet (or short paragraph) consistent with the section's
existing formatting.
Untestable: Structural constraint verified by visual inspection

### B6

WHEN any new bullet is added to a skill's "Rules" section as part
of this Work Item,
THE SYSTEM SHALL leave all other bullets in the section
unchanged.
Untestable: Structural constraint verified by visual inspection

### B7

WHEN this Work Item lands,
THE SYSTEM SHALL contain `Test:` references in
`documentation/behaviors.md` for each new EARS statement here,
covering the rule's presence in each of the four skills and
the consistency of wording across them. Tests are shell-based
grep checks (or equivalent) that the rule string is present.
Test: tests/behaviors/operations/test-easy-to-answer-skill-rule.sh (wording consistency across all four skills)

## DeleteLegacyRunModel

### B1

WHEN a user invokes `fluent run`, `fluent resume`, or
`fluent watch` after this Work Item lands,
THE SYSTEM SHALL NOT recognize these as valid subcommands;
they are absent from the Commands section of `fluent --help`.
Test: tests/binary.rs::deleted_subcommands_absent_from_help

### B2

WHEN the source tree is inspected after this Work Item lands,
THE SYSTEM SHALL NOT contain the files `src/run.rs`,
`src/session.rs`, `src/parallel.rs`, or the legacy
`src/merge.rs`.
Test: tests/binary.rs::no_legacy_run_strings_in_src (absence of `mod run;`, `mod session;`, `mod parallel;`, `mod merge;`)

### B3

WHEN any file under `src/` is inspected after this Work Item
lands,
THE SYSTEM SHALL NOT contain any references to `.fluent/runs`,
`active-run`, or `sessions.log`.
Test: tests/binary.rs::no_legacy_run_strings_in_src

### B4

WHEN any file under `prompts/` is inspected after this Work
Item lands,
THE SYSTEM SHALL contain only Work-model prompts
(`work-author.md`, `work-rebase.md`, `review-*.md`); the legacy
`author.md` and `behavior-tests.md` SHALL NOT be present.
Test: tests/binary.rs::no_legacy_prompt_files_in_prompts_dir

### B5

WHEN any documentation file under `documentation/` or skill
file under `skills/` is inspected after this Work Item lands,
THE SYSTEM SHALL NOT mention `.fluent/runs`, `fluent run`,
`fluent resume`, `fluent watch`, `sessions.log`,
`active-run`, `legacy fallback`, or `transitional bridge`.
Test: tests/binary.rs::no_legacy_run_strings_in_documentation

### B6

WHEN a user invokes `fluent status`,
THE SYSTEM SHALL show Work Items only (no legacy-Runs section)
and exit zero with the Work-state output the Work model produces.
Test: tests/binary.rs (status_shows_work_items_without_runs),
tests/behaviors/operations/test-work-status-dashboard.sh (status prints Work Items)

### B7

WHEN any test file under `tests/` is inspected after this Work
Item lands,
THE SYSTEM SHALL contain no test functions that exercise the
legacy CLI commands (`fluent run`, `fluent resume`,
`fluent watch`) or the legacy storage paths
(`.fluent/runs/`, `active-run`, `sessions.log`).
Test: tests/binary.rs::no_legacy_run_strings_in_src (forbidden substring scan covers test references indirectly via module-level grep)

### B8

WHEN CLAUDE.md is inspected after this Work Item lands,
THE SYSTEM SHALL NOT mention the legacy CLI commands or storage
paths; the "Fluent workflow" section SHALL describe Work-model
exclusively.
Test: tests/binary.rs::no_legacy_run_strings_in_documentation

### B9

WHEN the dashboard (`fluent dashboard`) renders after this Work
Item lands,
THE SYSTEM SHALL show only Work-Item state; the previous
legacy Runs view is removed entirely.
Test: dashboard::tests::test_work_view_renders_work_items,
tests/behaviors/operations/test-work-status-dashboard.sh (dashboard lists Work Items)

### B10

WHEN a `fluent <noun> <verb>` command is invoked,
THE SYSTEM SHALL behave identically to before this Work Item
lands. No Work-model behavior changes.
Test: all work_* tests in tests/binary.rs (pre-existing Work model test suite)

### B11

WHEN this Work Item lands,
THE SYSTEM SHALL contain `Test:` references in
`documentation/behaviors.md` for each EARS statement in the
DeleteLegacyRunModel section, covering both absence-of-legacy
assertions and CLI-error assertions.
Test: tests/binary.rs::no_legacy_run_strings_in_documentation (verifies behaviors.md does not reintroduce legacy strings)

## Usage logging

### B1

WHEN any Coder's run() completes (zero or non-zero exit),
THE SYSTEM SHALL read the Task's transcript file, extract every
turn-level token-usage event, and append one row per turn to
`~/.config/fluent/usage/usage.jsonl` with the documented
schema (ts, coder, work_item_id, attempt_id, task_id, model,
input_tokens, output_tokens, cached_input_tokens, and
reasoning_output_tokens for Codex only).
Test: src/usage.rs (extract_claude_usage_returns_one_row_per_result_event,
extract_codex_usage_returns_one_row_per_token_count_event,
extract_codex_usage_reads_turn_completed_usage,
extract_codex_usage_prefers_turn_completed_when_both_schemas_are_present,
append_rows_creates_parent_directory)

### B2

WHEN usage row appending fails (e.g., disk full, missing parent
directory, JSON encode error),
THE SYSTEM SHALL print a one-line warning to stderr and SHALL
NOT fail the Task or block the Coder's exit.
Test: src/usage.rs (log_usage_from_transcript wraps append_rows
and recompute_summary in warning-on-error; append_rows_creates_parent_directory)

### B3

WHEN a transcript contains zero parseable token events,
THE SYSTEM SHALL skip the append step silently (no warning).
Test: src/usage.rs (extract_claude_usage_returns_empty_when_no_result_events,
extract_codex_usage_skips_session_meta_and_response_item_events,
append_rows_is_no_op_for_empty_slice,
log_usage_from_transcript_skips_empty_transcript)

### B4

WHEN log_usage_from_transcript is called with an unknown coder type,
THE SYSTEM SHALL return immediately without extracting or appending
usage rows.
Test: src/usage.rs (log_usage_from_transcript_skips_unknown_coder)

### B5

WHEN usage rows are written to `usage.jsonl`,
THE SYSTEM SHALL update `~/.config/fluent/usage/summary.json`
to reflect new totals for `five_hour` and `weekly` windows per
coder. Window boundaries advance based on `ts` of incoming rows
(rows older than 5 hours / 7 days are excluded from the
respective spent calculations).
Test: src/usage.rs (recompute_summary_filters_by_five_hour_window,
recompute_summary_filters_by_weekly_window)

### B6

WHEN the summary file does not exist at write time,
THE SYSTEM SHALL create it with default zero spent and unset
remaining estimates (filled in by future calibration slices).
Test: src/usage.rs (recompute_summary_creates_zero_summary_when_log_missing)

## Queue substrate

### B1

WHEN `fluent queue add <work-item-id>` is invoked,
THE SYSTEM SHALL append a `queued` dispatch to the Work Item's ledger when no
dispatch is active, preserving earlier terminal history. If a dispatch is
already active, THE SYSTEM SHALL update its priority only when `--priority N`
was passed and otherwise preserve its fields and queue time.
Test: src/queue.rs (add_ready_work_creates_one_active_entry,
repeated_add_preserves_time_and_updates_only_explicit_priority)
Test: tests/binary.rs (work_queue_add_and_list_round_trip,
work_queue_add_existing_with_priority_updates_only_priority)

### B2

WHEN `fluent queue add` is invoked with an unknown
`work-item-id` (no matching `.fluent/work/items/<id>.json`),
THE SYSTEM SHALL exit non-zero with a clear error and SHALL NOT
create the queue file.
Test: src/queue.rs (add_fails_when_work_item_does_not_exist)
Test: tests/binary.rs (work_queue_add_unknown_work_item_errors)

### B2a

WHEN a Work Item's current Attempt is suspended at `needs-user`,
THE SYSTEM SHALL reject `fluent queue add` and name the exact
`fluent queue resume <work-item-id> --attempt <attempt-id>` command.
Test: tests/binary.rs (queue_add_rejects_suspended_attempt_and_pending_candidate)

### B2b

WHEN a Work Item has an older Attempt suspended at `needs-user` and a newer
planned Attempt, THE SYSTEM SHALL allow `fluent queue add` to queue the newer
Attempt and SHALL bind the scheduler dispatch to that planned Attempt.
Test: tests/binary.rs (queue_add_allows_planned_attempt_after_older_suspension)
Test: src/scheduler.rs (claim_binds_existing_current_planned_attempt)

### B2c

WHEN `fluent queue resume <work-item-id> --attempt <attempt-id>` names the
current `needs-user` Attempt and its pause kind is supported for recovery,
THE SYSTEM SHALL preserve the earlier terminal dispatch and append one queued
dispatch with durable `resume-attempt` intent, the exact Attempt binding, and a
link to the prior dispatch when one exists.
Test: src/queue.rs (resume_preserves_history_binds_exact_attempt_and_is_idempotent)
Test: tests/binary.rs (queue_resume_records_exact_authorization_and_is_idempotent)

### B2d

WHEN the same explicit resume command is repeated while its matching resume
dispatch is active, THE SYSTEM SHALL reuse that dispatch and SHALL NOT create a
second one; an explicit priority MAY update the active dispatch. A mismatched
Attempt, unsupported pause, or unrelated active or terminal dispatch SHALL fail
without changing the queue.
Test: src/queue.rs (resume_preserves_history_binds_exact_attempt_and_is_idempotent,
resume_rejects_wrong_or_unsupported_attempt_without_mutating_queue,
resume_rejects_unrelated_active_and_contradictory_terminal_dispatches)

### B2e

WHEN the scheduler claims a queued resume dispatch, THE SYSTEM SHALL run its
persisted Attempt binding without creating another Attempt. The authorization
SHALL survive coordinator restart; if execution pauses again, the new dispatch
SHALL end at `needs-user` and another explicit resume SHALL be required.
Test: src/scheduler.rs (explicit_resume_survives_restart_and_runs_exact_attempt,
explicit_resume_can_return_to_needs_user_without_losing_history)
Test: src/queue.rs (claim_accepts_explicit_resume_and_repeated_pause_needs_new_dispatch)

### B2f

WHEN status summarizes a supported current `needs-user` Attempt before it is
queued, THE SYSTEM SHALL name its exact queue-resume command. WHEN that resume
is queued and no coordinator is live, project status SHALL instead name
`fluent scheduler run` as the next action.
Test: src/work_status.rs (resumable_needs_user_status_names_exact_queue_resume_command)
Test: tests/binary.rs (queue_resume_records_exact_authorization_and_is_idempotent,
status_reports_queued_work_without_live_scheduler)
Evidence recovery SHALL offer queue resume only while current bound proof
permits it. A queued dispatch admitted before a later durable stop SHALL NOT
claim that Attempt; a new explicit resume must carry the current stop authority.
Test: tests/binary.rs (evidence_recovery_retries_preserve_task_and_dispatch_identity)
Test: tests/binary.rs (evidence_recovery_guidance_matches_foreground_and_queue_admission)

### B2g

WHEN `fluent queue resume <work-item-id> --attempt <attempt-id> --no-sandbox`
names the current `tester-harness` Attempt with a recorded Tester no-sandbox
recovery, THE SYSTEM SHALL append a resume dispatch that durably records the
exact Attempt binding and one Tester no-sandbox approval. The terminal dispatch
SHALL retain that approval as audit evidence.
Test: src/queue.rs (tester_no_sandbox_approval_is_durable_and_idempotent)
Test: src/scheduler.rs (tester_recovery_dispatch_forwards_one_run_no_sandbox_approval)

### B2h

IF `fluent queue resume ... --no-sandbox` names an Attempt without a recorded
Tester no-sandbox recovery, or an ordinary run dispatch carries Tester
no-sandbox approval, THEN THE SYSTEM SHALL reject it without changing or
claiming the queue entry.
Test: src/queue.rs (tester_no_sandbox_resume_rejects_non_tester_pause_without_mutation,
tester_no_sandbox_resume_rejects_an_unapproved_tester_pause,
tester_no_sandbox_claim_revalidates_the_tester_pause,
claim_rejects_no_sandbox_approval_on_ordinary_dispatch)
Test: tests/binary.rs (queue_resume_records_exact_authorization_and_is_idempotent)

### B2i

WHEN the scheduler runs an approved Tester no-sandbox resume dispatch, THE
SYSTEM SHALL retry only the exact paused Tester Task without Fluent's Tester
sandbox while preserving completed Writer Tasks.
Test: src/scheduler.rs (scheduler_child_uses_tester_only_capability_for_approved_recovery,
tester_recovery_dispatch_forwards_one_run_no_sandbox_approval)
Test: tests/binary.rs (tester_sandbox_recovery_reruns_same_tester_without_writer)

### B2j

WHILE an approved Tester no-sandbox resume is advancing the Attempt, THE SYSTEM
SHALL keep every other Work Task on its ordinary sandbox policy.
Test: src/work_attempt_loop.rs (tester_only_no_sandbox_capability_does_not_apply_to_other_tasks)

### B2k

WHEN a production Tester pauses because Xcode's embedded SwiftPM cannot create
its manifest sandbox inside Fluent's Tester sandbox, THE SYSTEM SHALL keep the
default sandbox enabled, SHALL NOT rewrite project configuration, and SHALL
write a needs-user handoff naming the exact `fluent queue resume ...
--no-sandbox` command and the resulting host-permission boundary.
Test: src/tester.rs (tester_diagnoses_nested_swiftpm_sandbox_failure)
Test: src/work_task_executor.rs (nested_swiftpm_handoff_names_exact_one_dispatch_approval)
Test: tests/binary.rs (tester_nested_sandbox_diagnosis_does_not_rewrite_project,
tester_sandbox_recovery_reruns_same_tester_without_writer)

### B2l

WHEN `fluent queue resume <work-item-id> --attempt <attempt-id>` names a
completed, review-passed Attempt whose Learner failed with a relaunchable
disposition, and every Task is Complete or valid Superseded terminal history,
exactly one pending unreviewed Merge Candidate still identifies the clean
candidate HEAD, and no Learner run is live, THE SYSTEM SHALL append a resume
dispatch bound to that Attempt.
Test: tests/binary.rs (learner_terminal_history_queue_resume_accepts_superseded)
Test: src/queue.rs (resume_queues_and_claims_only_the_retryable_failed_learner)

### B2m

WHEN the scheduler runs the resume dispatch from B2l, THE SYSTEM SHALL retry
only the Learner and preserve the Writer, Tester, reviewer, and Merge Candidate
records.
Test: tests/binary.rs (learner_terminal_history_scheduler_retries_only_learner)
Test: tests/binary.rs (scheduler_resume_retries_only_a_failed_learner)

### B2n

IF the Learner failure is not relaunchable, Task history is incomplete or
invalid, a Learner run is live, or the candidate is missing, ambiguous,
reviewed, landed, moved, or dirty, THEN THE SYSTEM SHALL reject the resume
without changing the queue.
Test: tests/binary.rs (learner_terminal_history_preserves_rejection_guards)
Test: src/queue.rs (resume_rejects_a_dirty_learner_retry_candidate_without_queue_mutation,
resume_rejects_a_non_relaunchable_learner_without_queue_mutation,
resume_rejects_a_live_learner_without_queue_mutation,
resume_rejects_an_incomplete_task_in_a_learner_retry,
resume_rejects_a_moved_learner_retry_candidate,
resume_rejects_a_reviewed_or_landed_learner_candidate,
resume_rejects_a_missing_or_ambiguous_learner_candidate)

### B2o

IF a candidate no longer satisfies B2l when the scheduler claims its admitted
resume, THEN THE SYSTEM SHALL reject the claim and leave the dispatch queued.
Test: tests/binary.rs (learner_terminal_history_claim_revalidates_current_tasks)
Test: src/queue.rs (claim_rejects_a_learner_candidate_dirtied_after_resume)

### B2p

WHEN a Task pause supersedes an earlier pause after the Attempt resumes, THE
SYSTEM SHALL replace the Attempt's managed current needs-user handoff.
Test: src/work_task_executor.rs (tester_pause_replaces_a_resumed_round_cap_current_handoff)

### B2q

WHEN Fluent replaces an Attempt's managed current needs-user handoff, THE
SYSTEM SHALL preserve each Task-specific handoff as history.
Test: src/work_task_executor.rs (tester_pause_replaces_a_resumed_round_cap_current_handoff)
Test: tests/binary.rs (work_task_run_preserves_task_handoff_and_updates_current)

### B2r

WHEN status or Work Item show reports a current Tester no-sandbox recovery, THE
SYSTEM SHALL name the exact `fluent queue resume <work-item-id> --attempt
<attempt-id> --no-sandbox` command.
Test: src/work_status.rs (tester_sandbox_recovery_status_names_the_exact_approved_command,
tester_sandbox_recovery_show_names_the_exact_approved_command)
Test: tests/binary.rs (tester_sandbox_recovery_reruns_same_tester_without_writer)

### B2s

IF Fluent cannot publish the managed current handoff after recording its pause,
THEN THE SYSTEM SHALL preserve the pause and return the publication failure.
Test: src/work_task_executor.rs (current_handoff_failure_surfaces_after_the_pause_is_durable)

### B2t

IF Fluent cannot publish the managed current handoff after recording its pause,
THEN THE SYSTEM SHALL NOT notify the user that the recovery action was recorded.
Test: src/work_task_executor.rs (current_handoff_failure_surfaces_after_the_pause_is_durable)

### B2u

WHEN an Attempt has an evidence-targeted Review that is planned or completed
but not yet consumed by the Attempt loop, THE SYSTEM SHALL reject ordinary
`fluent queue add` before it can create another Attempt and SHALL accept
`fluent queue resume ... --attempt <that-attempt>` after a blocked dispatch.
Test: src/queue.rs (add_rejects_pending_evidence_review_instead_of_creating_new_attempt,
resume_accepts_pending_evidence_review_after_blocked_dispatch,
resume_accepts_completed_unsettled_evidence_review_after_blocked_dispatch)

### B2v

IF an older Fluent dispatch already created a later initial Writer while that
evidence Review remained pending, and the later Writer is interrupted without
output in a clean worktree still at project HEAD, THEN the exact public resume
SHALL preserve it as superseded history and bind the new dispatch to the
evidence Review's Attempt. IF that recovered Attempt later pauses again, THEN
another exact public resume SHALL continue to accept the recovered Attempt
without restoring the superseded later Attempt as current.
Test: src/queue.rs (resume_supersedes_clean_interrupted_accidental_queue_admission)

### B2w

IF that later Writer has a live owner or candidate work, THEN THE SYSTEM SHALL
reject reconciliation without changing either Attempt or the queue.
Test: src/queue.rs (resume_supersedes_clean_interrupted_accidental_queue_admission)

### B3

WHEN `fluent queue list` is invoked,
THE SYSTEM SHALL print each active queue entry on its own line, sorted
by `priority` descending then `queued_at` ascending, in the
format `<priority> <queued_at> <status> <work-item-id>`.
Test: src/queue.rs (list_sorts_by_priority_then_queue_time)
Test: tests/binary.rs (work_queue_list_format_includes_priority_queued_at_status_id)

### B3a

WHEN `fluent queue show <work-item-id>` is invoked for a Work Item with dispatch
history, THE SYSTEM SHALL print its latest dispatch as JSON, including its
terminal status and bound Attempt when present.
Test: src/queue.rs (show_returns_latest_terminal_dispatch_with_attempt_binding)
Test: tests/binary.rs (work_queue_show_prints_latest_terminal_dispatch_as_json)

### B3b

IF `fluent queue show <work-item-id>` finds no dispatch history, THEN THE SYSTEM
SHALL exit non-zero with a clear error.
Test: src/queue.rs (show_reports_missing_dispatch)
Test: tests/binary.rs (work_queue_show_missing_dispatch_errors)

### B4

WHEN `fluent queue remove <work-item-id>` is invoked and
an unclaimed dispatch is active,
THE SYSTEM SHALL record a terminal `canceled` disposition and exit zero. If no
active dispatch exists, it SHALL exit non-zero with a clear "not queued"
message; a live claim SHALL reject removal.
Test: src/queue.rs (remove_unclaimed_entry_preserves_cancellation_disposition,
remove_missing_entry_reports_not_queued, remove_rejects_live_claim)
Test: tests/binary.rs (work_queue_remove_after_add_removes_entry,
work_queue_remove_unknown_errors)

## Scheduler

### Shared service lifecycle

WHEN a shared service answers health with a compatible protocol, a verifiable
current lease and metadata, and the requested version and executable SHA-256,
THE SYSTEM SHALL reuse that service through both direct and managed startup.
Test: tests/scheduler_service.rs (scheduler_host_upgrade_upgrade_reuses_matching_service,
scheduler_host_upgrade_managed_start_checks_observed_build)

IF either build field differs, THE SYSTEM SHALL reject reuse, report both
builds, and direct the operator to run `fluent scheduler stop --all` before
explicitly starting the new build. These checks also apply after a newly
started managed service answers health.
Test: tests/scheduler_service.rs (scheduler_host_upgrade_upgrade_rejects_mismatched_build,
scheduler_host_upgrade_managed_start_checks_observed_build)

IF the live service is draining or its identity cannot be verified, THE SYSTEM
SHALL reject reuse without replacing its socket or starting another owner.
Test: tests/scheduler_service.rs (scheduler_host_upgrade_upgrade_preserves_unverified_owner,
scheduler_host_upgrade_preserves_bad_health_socket,
scheduler_host_upgrade_simultaneous_starts_preserve_owner)

WHEN the identified service accepts `fluent scheduler stop --all`, THE SYSTEM
SHALL stop making new dispatch claims across all registered projects.
Test: tests/scheduler_service.rs (scheduler_drain_stops_claiming_all_projects,
scheduler_host_upgrade_drains_two_projects_through_socket)

WHILE the shared service is draining, THE SYSTEM SHALL reject new execution
submissions without persisting their requests or dispatches.
Test: tests/scheduler_service.rs (scheduler_drain_rejects_new_submissions,
scheduler_host_upgrade_drains_two_projects_through_socket)

WHEN every worker owned by the draining service has settled, THE SYSTEM SHALL
reconcile their results before publishing an instance-specific stop result and
releasing ownership.
Test: tests/scheduler_service.rs (scheduler_host_upgrade_drains_two_projects_through_socket,
scheduler_host_upgrade_preserves_settled_worker_after_child_crash)
Test: src/scheduler_service.rs (stop_result_tests::rejects_published_result_when_lease_identity_is_unreadable)

WHEN a stop request repeats for the same instance, THE SYSTEM SHALL return its
retained result after ownership release.
Test: tests/scheduler_service.rs (scheduler_host_upgrade_drain_restart_and_replay,
scheduler_host_upgrade_replays_published_result_after_child_crash,
scheduler_drain_replays_same_instance)

IF a stop request names an earlier instance, THEN THE SYSTEM SHALL leave its
replacement running.
Test: tests/scheduler_service.rs (scheduler_host_upgrade_drain_restart_and_replay,
scheduler_host_upgrade_rejects_old_drain_after_ack_crash)

WHEN the old instance completes its drain and the operator starts a new build,
THE SYSTEM SHALL keep queued work available for that new instance.
Test: tests/scheduler_service.rs (scheduler_host_upgrade_drain_restart_and_replay,
scheduler_host_upgrade_drains_two_projects_through_socket)

IF a reachable legacy service lacks verifiable drain control, THE SYSTEM SHALL
identify the running build and direct the operator to stop its original
supervised process, wait for active work to settle, then start the new build.
Test: tests/scheduler_service.rs (scheduler_host_upgrade_upgrade_preserves_unverified_owner)

WHEN an operator runs `fluent scheduler stop` without `--all`, THE SYSTEM SHALL
stop only that project's coordinator.
Test: tests/scheduler_service.rs (scheduler_upgrade_project_stop_scope)

### B1

WHEN `fluent scheduler run` is invoked,
THE SYSTEM SHALL elect one project coordinator and fill available local
capacity from queued dispatches by priority descending and queue time
ascending. Each claim SHALL durably bind one Attempt before launch, transition
through `claimed` and `running`, and invoke `fluent attempt run` for that exact
Attempt.
Test: src/scheduler.rs (concurrent_starts_elect_one_coordinator,
scheduler_fills_available_project_capacity,
scheduler_orders_priority_then_fifo_without_preemption,
claim_persists_bound_attempt_before_launch,
bound_claim_transitions_to_running_on_launch)
Test: tests/binary.rs (work_scheduler_run_processes_queued_work_item_end_to_end,
scheduler_runs_four_work_items_concurrently_by_default)

### B2

WHEN the running Attempt terminates,
THE SYSTEM SHALL update the queue entry's `status` to:

- `candidate-ready` if the Attempt completes with a passing Merge Candidate
- `failed` if the Attempt ends in a `failed` terminal state
- `needs-user` if the Attempt ends in `needs-user`
- `blocked` if the Attempt is complete but its Merge Candidate has not passed
  Learner advancement

The dispatch SHALL NOT be deleted; the user inspects active work through
`fluent queue list` and the latest terminal history through `fluent queue show`.
Test: src/scheduler.rs (concurrent_terminal_outcomes_are_independent,
passing_attempt_becomes_candidate_ready_without_land,
complete_attempt_without_succeeded_learner_is_blocked)

### B3

WHEN the running Attempt terminates,
THE SYSTEM SHALL NOT invoke merge logic itself. Merging is the
job of `fluent auto-merge --all`, which the user runs
separately as a sibling long-running process.
Test: src/scheduler.rs (scheduler_never_invokes_merge,
passing_attempt_becomes_candidate_ready_without_land)

### B4

WHEN the scheduler's poll finds no `queued` entries,
THE SYSTEM SHALL sleep for a configurable interval (default 30
seconds) before re-polling.
Test: tests/binary.rs (work_scheduler_run_exits_clean_on_sigterm_when_idle)

### B5

WHEN the scheduler receives SIGTERM or SIGINT while idle (no
Attempt currently running),
THE SYSTEM SHALL exit zero immediately.
Test: tests/binary.rs (work_scheduler_run_exits_clean_on_sigterm_when_idle)

### B6

WHEN the scheduler receives SIGTERM or SIGINT while an Attempt
is mid-execution,
THE SYSTEM SHALL allow the Attempt to complete (do not interrupt
the child `fluent attempt run` process), update the queue
entry's `status` accordingly, then exit zero.
Test: src/scheduler.rs (graceful_shutdown_drains_claimed_work,
shutdown_stops_claiming_and_preserves_unclaimed_work)

### B7

WHEN the scheduler reads a queue file that fails to parse,
THE SYSTEM SHALL skip that entry, log a one-line warning to
stderr, and continue with the next entry. The malformed file
SHALL NOT be deleted automatically.
Test: src/queue.rs (scheduler_warns_and_preserves_malformed_entry,
scheduler_skips_malformed_entry_without_stalling)

### B8

WHEN `fluent scheduler status` is invoked with no live project coordinator,
THE SYSTEM SHALL report that the scheduler is stopped without changing
scheduler state.
Test: src/scheduler.rs (stopped_status_does_not_create_scheduler_state)
Test: tests/binary.rs (scheduler_status_and_stop_use_exact_live_instance)

### B9

WHEN `fluent scheduler status` is invoked with a live project coordinator,
THE SYSTEM SHALL report its instance, running or draining state, start time,
display-only process id, Fluent version, and executable SHA-256 without changing
scheduler state.
Test: src/scheduler.rs (status_reads_live_identity_without_mutation)
Test: tests/binary.rs (scheduler_status_and_stop_use_exact_live_instance)

### B10

WHEN `fluent scheduler run` finds a running coordinator with the same version
and executable SHA-256, THE SYSTEM SHALL reuse it and name that build.
Test: src/scheduler.rs (same_build_reuse_reports_the_elected_identity)
Test: tests/binary.rs (second_scheduler_run_reuses_live_coordinator)

### B11

IF `fluent scheduler run` finds a coordinator that is already draining, THEN
THE SYSTEM SHALL reject reuse and tell the operator to wait before running the
scheduler again.
Test: src/scheduler.rs (run_does_not_reuse_a_draining_coordinator)

### B12

IF `fluent scheduler run` finds a live coordinator with a different version or
executable SHA-256, THEN THE SYSTEM SHALL reject reuse and direct the operator to
run `fluent scheduler stop` followed by `fluent scheduler run`.
Test: src/scheduler.rs (different_build_reuse_requires_explicit_stop_then_run)

### B13

WHEN `fluent scheduler stop` targets a live identified coordinator, THE SYSTEM
SHALL change that coordinator to `draining` and stop it from claiming new Work.
Test: src/scheduler.rs (matching_stop_request_publishes_draining_before_exit,
shutdown_stops_claiming_and_preserves_unclaimed_work)
Test: tests/binary.rs (scheduler_status_and_stop_use_exact_live_instance)

### B14

WHILE the coordinator targeted by `fluent scheduler stop` has running Work, THE
SYSTEM SHALL let that Work finish before releasing the coordinator lease and
reporting the stopped instance.
Test: src/scheduler.rs (durable_stop_request_drains_and_releases_exact_coordinator,
graceful_shutdown_drains_claimed_work)
Test: tests/binary.rs (scheduler_status_and_stop_use_exact_live_instance)

### B15

IF a live coordinator has missing, malformed, unsupported, or lease-mismatched
identity metadata, THEN THE SYSTEM SHALL reject status, reuse, and durable stop
requests without signaling the recorded process id.
Test: src/scheduler.rs (live_coordinator_with_malformed_metadata_fails_closed,
live_legacy_coordinator_without_metadata_fails_closed,
metadata_from_an_earlier_lease_fails_closed)

### B16

IF a durable stop request names an earlier coordinator instance, THEN THE SYSTEM
SHALL keep the replacement coordinator running.
Test: src/scheduler.rs (stale_stop_request_cannot_stop_replacement_instance)

### B17

WHEN a foreground `fluent attempt run` advances an Attempt that is bound to a
queued or `needs-user` dispatch, THE SYSTEM SHALL reconcile that dispatch from
the same Attempt outcome before the command returns. If the Attempt is
reopened and remains planned without an executing Task, THE SYSTEM SHALL
return the bound dispatch to `queued`; a live `claimed` or `running` dispatch
remains owned by its scheduler worker.
Test: src/scheduler.rs (queued_resume_reconciles_after_foreground_attempt_finishes)
Test: src/scheduler.rs (suspended_dispatch_requeues_after_foreground_reopen)

## Scheduler cross-cutting

### B1

WHEN this Work Item lands,
THE SYSTEM SHALL contain `Test:` references in
`documentation/behaviors.md` for each new EARS statement here,
covering usage row appending, queue CRUD operations, the
scheduler's pick-and-run loop, signal handling, and the
no-merge-from-scheduler rule.
Test: all EARS statements in the Usage logging, Queue substrate,
and Scheduler sections above carry `Test:` references

## Reviewer transcript persistence cross-cutting

### B1

WHEN this Work Item lands,
THE SYSTEM SHALL contain `Test:` references in
`documentation/behaviors.md` for each new EARS statement here,
covering transcript-file presence checks for write and review
Tasks, the independent-verification regression test (reviewer
cannot read writer transcript or other reviewers' transcripts),
and the usage-logging integration.
Test: all EARS statements in the reviewer transcript persistence
section above carry `Test:` references

## Pi as a third CoderKind

THE SYSTEM SHALL recognize `pi` as a valid Coder kind alongside
`claude` and `codex`. Specifying `pi` via `--coder`,
`FLUENT_CODER`, or a coder mapping entry SHALL be accepted.
Error messages, help output, and any documentation of available
Coders SHALL list `pi`.
Test: src/coder.rs (coder_kind_resolves_pi)
Test: src/coder.rs (coder_kind_serializes_pi_as_kebab_case)

## Coder selection

### B1

WHEN Fluent creates an Attempt and resolves the coder, model, and reasoning
effort for a writer or reviewer role,
THE SYSTEM SHALL take each from the first source that provides it: a
`--write-*` or `--review-*` command-line override, then a global command-line
`--coder`, `--model`, or `--effort` override, then an env var, then the project
`.fluent/config.yaml`, then the user
`~/.config/fluent/config.yaml`.
Test: src/work_model.rs (resolve_coder_mapping_precedence_flag_env_config)
Test: src/work_model.rs (merge_cli_global_model_fills_unset_roles)
Test: src/work_model.rs (merge_cli_per_role_effort_wins_over_global)

### B2

WHEN `fluent attempt create` is given `--coder <kind>` or `--model <model>`,
THE SYSTEM SHALL apply it to that attempt, overriding config and env.
Test: tests/binary.rs (attempt_coder_model_flags_override_config)

### B3

WHEN an attempt is about to launch its first round,
THE SYSTEM SHALL print the resolved coder and model (and effort, when set) for each
role before any coder starts. It SHALL suppress this under `FLUENT_QUIET`.
Test: tests/binary.rs (attempt_run_prints_resolved_coder_plan)

### B4

WHERE no model is configured or overridden for a role,
THE SYSTEM SHALL NOT pass a model to that coder; the coder CLI uses its own default.
Test: src/coder.rs (claude_command_omits_model_when_unset)
Test: src/coder.rs (codex_command_omits_model_when_unset)

## Per-Attempt coder mapping

### B1

WHEN Fluent executes a Writer or reviewer Task within an Attempt,
THE SYSTEM SHALL invoke the Coder from that role's mapping on the Attempt.
Test: src/work_model.rs (coder_mapping_for_task_kind_returns_correct_pair)

### B1a

WHEN Fluent executes a Tester Task within an Attempt,
THE SYSTEM SHALL run the deterministic Tester without spawning a Coder.
Test: src/work_task_executor.rs (tester_task_invokes_subcommand_not_coder)

### B2

WHEN Fluent creates an Attempt with `--coder X` or `FLUENT_CODER=X` set and no
per-role mapping specified,
THE SYSTEM SHALL set both role mappings to Coder X.
Test: src/work_model.rs (resolve_coder_mapping_fluent_coder_sets_both_roles)

### B3

IF an Attempt is created with no coder configuration,
THEN THE SYSTEM SHALL set the Attempt's coder mapping to the
user-configured default mapping, falling back to Claude for every
role when no user default is configured.
Test: src/work_model.rs (resolve_coder_mapping_default_when_nothing_set)

### B4

WHEN Fluent invokes a Coder for a Task,
THE SYSTEM SHALL invoke the Coder with the model from the
Attempt role mapping selected for that Task kind.
Test: src/work_model.rs (resolve_coder_mapping_stores_resolved_model_at_creation)

### B5

WHEN Fluent creates an Attempt and a role mapping
has no explicit model,
THE SYSTEM SHALL resolve the model from the per-Coder environment
variable (`FLUENT_CLAUDE_MODEL`, `FLUENT_CODEX_MODEL`,
`FLUENT_PI_MODEL`) or the Coder's built-in default, and store
the resolved model in the mapping entry.
Test: src/work_model.rs (resolve_coder_mapping_per_role_cli_flag_wins)

### B6

WHEN Fluent creates an Attempt,
THE SYSTEM SHALL store the Attempt's coder mapping on the Attempt
record so it can be inspected via `fluent work-item show <work-item-id>`.
Test: src/work_model.rs (coder_mapping_round_trips_json)
Test: src/work_model.rs (attempt_with_coder_mapping_round_trips)

### B7

WHEN the scheduler claims queued Work that has no existing Attempt,
THE SYSTEM SHALL create that Attempt with the same writer and reviewer mapping
that `fluent attempt create` would resolve from the current configuration and
supported environment overrides.
Test: src/scheduler.rs (scheduler_created_attempt_snapshots_project_coder_mapping)
Test: src/scheduler.rs (claim_persists_bound_attempt_before_launch)
Test: src/config.rs (attempt_creation_inputs_layer_environment_over_project_config)

### B8

WHEN the scheduler claims or resumes a dispatch already bound to an existing
Attempt,
THE SYSTEM SHALL preserve that Attempt's stored coder mapping even when current
project configuration differs.
Test: src/scheduler.rs (scheduler_preserves_existing_attempt_coder_mapping)
Test: src/scheduler.rs (explicit_resume_survives_restart_and_runs_exact_attempt)

## Attempt coder mapping continuity

### B1

WHEN `fluent attempt run <work-item-id> <attempt-id>` is invoked without an
explicit coder, model, or effort flag,
THE SYSTEM SHALL execute the Attempt using its stored coder mapping and leave
that mapping unchanged even when current configuration or environment values
differ.
Test: tests/binary.rs (attempt_run_without_overrides_preserves_stored_coder_mapping)

### B2

WHEN `fluent task run <work-item-id> <attempt-id> <task-id>` is invoked without
an explicit coder, model, or effort flag,
THE SYSTEM SHALL execute the Task using the matching entry in the Attempt's
stored coder mapping and leave every mapping entry unchanged.
Test: tests/binary.rs (task_run_without_overrides_preserves_stored_coder_mapping)

### B3

WHEN `fluent attempt run` or `fluent task run` is invoked with one or more
explicit coder, model, or effort flags,
THE SYSTEM SHALL apply only those supplied fields over the Attempt's stored
coder mapping, preserve every field not overridden, and store the resulting
mapping on the Attempt.
Test: tests/binary.rs (attempt_run_overrides_only_explicit_coder_mapping_fields)
Test: tests/binary.rs (task_run_overrides_only_explicit_coder_mapping_fields)

### B4

WHEN `fluent attempt run` resumes an Attempt whose next action is a Learner-only
retry or recovery,
THE SYSTEM SHALL invoke the Learner with the applicable entry from that
Attempt's stored coder mapping unless the command supplies an explicit run
override.
Test: src/work_attempt_loop.rs (learner_only_resume_uses_attempt_coder_mapping)

## Pi invocation mechanics

### B1

WHEN Fluent invokes a Coder for a Task,
THE SYSTEM SHALL capture the Coder's version in the Task's
artifact area.
Test: src/work_task_executor.rs (capture_coder_info_writes_json)

### B2

WHEN Fluent invokes Pi for a Task and a transcript path is
configured,
THE SYSTEM SHALL run Pi in JSONL streaming mode (`--mode json`)
and write Pi's stdout to that transcript path.
Untestable: verified by subprocess invocation integration test;
Pi subprocess is not mockable in unit tests.

### B3

WHEN Fluent invokes Pi for a Task,
THE SYSTEM SHALL pass an explicit `--thinking <level>` flag (off,
low, medium, or high) rather than relying on Pi's default.
Untestable: verified by code inspection of PiCode::run() which
always passes `--thinking off`.

## Transcript parsing

### B1

WHEN Fluent parses a Pi transcript after Task completion,
THE SYSTEM SHALL emit one usage row per completed Pi turn
(identified by a `turn_end` event), with `input_tokens` and
`output_tokens` taken from `message.usage.input` and
`message.usage.output`.
Test: src/usage.rs (extract_pi_usage_returns_one_row_per_turn_end_event)
Test: src/usage.rs (extract_pi_usage_skips_non_turn_end_events)

### B2

WHEN Fluent parses a Pi transcript after Task completion,
THE SYSTEM SHALL populate each usage row's `model` field from the
`message.model` value on the corresponding `turn_end` event.
Test: src/usage.rs (extract_pi_usage_populates_model_from_message_model)

### B3

WHEN Fluent parses a Pi transcript after Task completion,
THE SYSTEM SHALL populate each usage row's `duration_ms` field as
the difference between the `turn_end` event's `message.timestamp`
(epoch ms) and the previous `turn_end` event's `message.timestamp`
(or the `session` event's `timestamp` for the first turn). When
no anchor timestamp is available (no `session` event and no
previous `turn_end`), `duration_ms` SHALL be `Some(0)`. All
durations SHALL be expressed in milliseconds.
Test: src/usage.rs (extract_pi_usage_first_turn_duration_anchored_to_session_timestamp)
Test: src/usage.rs (extract_pi_usage_subsequent_turn_duration_uses_previous_turn_end)
Test: src/usage.rs (extract_pi_usage_first_turn_duration_zero_when_session_missing)

### B4

WHEN Fluent parses a Claude transcript after Task completion,
THE SYSTEM SHALL populate each usage row's `model` field from the
`system` event with `subtype: "init"` (the session-initialization
event that names the model the session was launched with).
Test: src/usage.rs (extract_claude_usage_populates_model_from_system_init)
Test: src/usage.rs (extract_claude_usage_falls_back_to_unknown_when_init_missing)

### B5

WHEN Fluent parses a Pi transcript after Task completion,
THE SYSTEM SHALL extract per-turn token usage from Pi's JSONL
events and append usage rows to
`~/.config/fluent/usage/usage.jsonl`, identifying the Task via
`work_item_id`, `attempt_id`, and `task_id`.
Test: src/usage.rs (extract_pi_usage_returns_one_row_per_turn_end_event)
Test: src/usage.rs (extract_pi_usage_returns_empty_when_no_turn_end_events)

### B6

WHEN Fluent parses a Coder transcript after Task completion,
THE SYSTEM SHALL extract per-turn end-to-end duration (and any
other available timing signals) from the transcript and include
them in the appended usage rows for Claude, Codex, and Pi.
Test: src/usage.rs (usage_row_serializes_with_duration_ms)
Test: src/usage.rs (extract_claude_usage_populates_duration_ms)

## Pi coder cross-cutting

### B1

WHEN this Work Item lands,
THE SYSTEM SHALL contain `Test:` references in
`documentation/behaviors.md` for each new EARS statement here,
covering Pi coder kind recognition, per-Attempt coder mapping
resolution, coder version capture, Pi transcript parsing, and
cross-coder timing extraction.
Test: all EARS statements in the Pi coder sections above carry
`Test:` or `Untestable:` references

## Plan execution: progress.md

### B1

WHEN a write Task runs within an Attempt,
THE SYSTEM SHALL designate the path
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/progress.md`
for progress.md alongside other Attempt-level artifacts. Progress.md
SHALL NOT be tracked by git.
Test: src/work_model.rs (progress_md_in_reviewer_input_artifacts)

### B2

WHEN a write Task creates or updates progress.md,
THE SYSTEM SHALL maintain two sections in the file: a top
`## Checklist` section containing one `- [ ]` or `- [x]` line per
plan.md step (in the same order as plan.md's step list), and a
`## Notes` section containing per-step `### Step N` subsections
with `Done`, `Note`, and `Next` lines for each completed step.
Untestable: Writer protocol is prompt-driven convention, not
enforced by code

### B3

WHEN a writer Task starts and progress.md does not yet exist at
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/progress.md`,
THE SYSTEM SHALL create progress.md with a `## Checklist` section
seeded with one `- [ ]` line per plan.md step (in plan.md order)
and an empty `## Notes` section.
Untestable: Writer protocol is prompt-driven convention, not
enforced by code

### B4

WHEN a writer Task starts a new step (at session start, or
after completing the previous step),
THE SYSTEM SHALL read progress.md, find the first `- [ ]` item in
the Checklist section, and treat that as the next step to work on.
Untestable: Writer protocol is prompt-driven convention, not
enforced by code

### B5

WHEN a writer Task commits code changes for a step,
THE SYSTEM SHALL update progress.md after the commit: toggle the
step's Checklist item from `- [ ]` to `- [x]`, and append a
`### Step N` subsection under Notes containing `Done` (including
the commit hash of the just-made commit), `Note`, and `Next` lines.
Untestable: Writer protocol is prompt-driven convention, not
enforced by code

### B6

WHEN a reviewer Task runs for an Attempt that has a
progress.md file in its artifact area,
THE SYSTEM SHALL include progress.md as a readable input in the
reviewer's sandbox, and the reviewer prompt SHALL name progress.md
as a relevant input artifact.
Test: src/work_model.rs (progress_md_in_reviewer_input_artifacts)

### B7

WHEN the behaviors-completeness reviewer runs for an Attempt
with both plan.md and progress.md present,
THE SYSTEM SHALL check that every plan.md step appears as a
Checklist item in progress.md, and that the review verdict
reflects whether all items are `- [x]`.
Test: review-behaviors prompt contains cross-check instruction

### B8

WHEN a Writer completes a code-producing round,
THE SYSTEM SHALL instruct it to run focused harness-native checks instead of the
complete configured suite that belongs to Fluent's final Tester gate.
Test: src/work_task_executor.rs (writer_prompt_assigns_focused_checks_and_reports_them_for_review)

### B9

WHEN a Writer reports its focused verification,
THE SYSTEM SHALL require the exact command, result, and covered risk for each
check, plus the canonical commands left to Fluent's final Tester.
Test: src/work_task_executor.rs (writer_prompt_assigns_focused_checks_and_reports_them_for_review)

## Tester Task

### B1

WHEN all scheduled reviewers pass a code-producing candidate,
THE SYSTEM SHALL plan exactly one `TaskKind::Tester` Task with id
`<attempt-id>-tester` (or `<attempt-id>-tester-<round>` for review rounds beyond
the first), bound to that exact reviewed candidate commit, before Learning or
Merge Candidate readiness.
Test: src/work_model.rs (final_tester_is_bound_to_the_reviewed_candidate_after_review)
Test: src/work_attempt_loop.rs (passing_code_reviews_plan_one_final_tester_before_candidate_creation)

Evidence-targeted review after a complete canonical run SHALL NOT create a
duplicate Tester when the run and reviewed replacements satisfy every required
check for the candidate.
Test: tests/binary.rs (canonical_tester_recovery_reuses_other_passing_results)

### B2

WHEN `TaskKind::Tester` is serialized and deserialized,
THE SYSTEM SHALL round-trip as `"tester"`.
Test: src/tester.rs (task_kind_tester_round_trips)

### B3

WHEN reviewer Tasks are scheduled for a code-producing Attempt round,
THE SYSTEM SHALL make every reviewer ready without a Tester dependency.
Test: src/work_model.rs (code_review_tasks_do_not_plan_a_tester_before_review)

### B4

IF the writer Task fails before producing a candidate workspace,
THEN no Tester Task SHALL be created for that round.
Test: src/work_model.rs (no_tester_task_when_writer_failed)

### B5

WHEN a `TaskKind::Tester` Task is executed,
THE SYSTEM SHALL invoke the `fluent` binary's Tester subcommand
against the candidate workspace, without spawning any Coder process
for that Task.
Test: src/work_task_executor.rs (tester_task_invokes_subcommand_not_coder)

### B6

WHEN the Tester subcommand runs,
THE SYSTEM SHALL read `.fluent/tester.yaml` from the candidate
workspace, run each declared command in the candidate workspace
sequentially, and capture each command's stdout, stderr, exit code,
and wall-clock duration.
Test: src/tester.rs (tester_runs_declared_commands_sequentially)
Test: src/tester.rs (tester_captures_stdout_stderr_exit_duration)

### B7

WHEN the Tester subcommand has captured all command outputs,
THE SYSTEM SHALL invoke `.fluent/extract-tester-results` with the
captured per-command data, receive a normalized per-test array back,
assemble a `tester-results.json` artifact (containing `commands`,
`tests`, `summary`, and `error`), and write it to the Tester Task's
artifact directory.
Test: src/tester.rs (tester_invokes_extractor_and_writes_results_json)

### B8

IF `.fluent/tester.yaml` is missing, unreadable, or malformed,
THEN the Tester subcommand SHALL produce `tester-results.json` with
a top-level `error` field describing the problem, empty `commands`
and `tests` arrays, and a zero summary. The Tester Task SHALL
complete successfully.
Test: src/tester.rs (tester_soft_fails_when_tester_yaml_missing)
Test: src/tester.rs (tester_soft_fails_when_tester_yaml_malformed)

### B9

IF `.fluent/extract-tester-results` is missing or not executable,
THEN the Tester subcommand SHALL produce `tester-results.json` with
a top-level `error` field describing the problem, the captured
`commands` array, an empty `tests` array, and a zero summary. The
Tester Task SHALL complete successfully.
Test: src/tester.rs (tester_soft_fails_when_extractor_missing)
Test: src/tester.rs (tester_soft_fails_when_extractor_not_executable)

### B10

WHEN a command declared in `tester.yaml` exits non-zero,
THE SYSTEM SHALL still run subsequent declared commands AND SHALL
still produce `tester-results.json` containing each command's exit
info. The Tester Task itself SHALL complete successfully — individual
command failures are data the Attempt loop interprets at the final gate, not
Tester infrastructure errors.
Test: src/tester.rs (tester_continues_when_command_exits_nonzero)
Test: src/tester.rs (tester_task_succeeds_when_individual_commands_fail)

### B10a

WHEN a Work Task Tester command changes a tracked file in the clean candidate
workspace without changing the candidate commit,
THE SYSTEM SHALL preserve the command result and logs, record the changed paths,
restore the candidate's tracked index and worktree to that commit before another
Tester execution or lifecycle gate, and leave untracked and ignored outputs
unchanged.
Test: tests/binary.rs (work_task_run_tester_restores_tracked_candidate_mutations_and_keeps_evidence)
Test: tests/binary.rs (work_task_run_tester_recovers_when_error_is_transient)

### B10b

IF the candidate has tracked changes before a Work Task Tester starts, or its
HEAD changes during execution,
THEN THE SYSTEM SHALL fail closed without overwriting that state.
Test: src/work_task_executor.rs (tester_candidate_guard_rejects_preexisting_tracked_changes_without_restoring_them)
Test: src/work_task_executor.rs (tester_candidate_guard_does_not_rewind_a_head_change_during_execution)

### B10c

WHEN an explicit resume encounters a final-Tester gate created by an older
Fluent build,
THE SYSTEM SHALL restore the tracked mutation only if the gate's exact dirty
status, completed Tester, candidate commit, and current tracked status still
match; otherwise it SHALL leave the candidate unchanged.
Test: src/work_attempt_loop.rs (explicit_final_tester_gate_resume_restores_matching_legacy_tracked_mutation)

### B10d

WHEN a project-owned Tester harness failure requires a tracked configuration or
extractor repair, THE SYSTEM SHALL name an eligible `fluent attempt
repair-tester` command with the affected Work Item and Attempt.
Test: tests/binary.rs (tester_harness_repair_public_workflow_is_audited_and_idempotent)

### B10e

WHEN an operator approves a repair from the exact clean Tester candidate, THE
SYSTEM SHALL create one audited repair Writer bound to the decision, that
candidate, and the failed Tester evidence.
Test: src/tester_harness_recovery.rs (request_records_repair_writer_with_source_provenance)

### B10f

WHEN the repair Writer completes, THE SYSTEM SHALL accept a commit only when its
net changes are confined to `.fluent/tester.yaml` and
`.fluent/extract-tester-results`.
Test: src/work_task_executor.rs (tester_harness_repair_scope_accepts_only_config_and_extractor)
Test: src/work_task_executor.rs (tester_harness_repair_writer_restores_rejection_and_retries_through_real_executor)

### B10g

WHEN Fluent accepts a Tester repair commit, THE SYSTEM SHALL bind subsequent
review and Tester evidence to an Attempt history that retains the failed Tester,
original candidate, and repair commit.
Test: src/tester_harness_recovery.rs (repaired_writer_output_pins_fresh_review_and_tester_contexts)

### B10h

IF the repair candidate is dirty or its HEAD differs from the failed Tester's
pinned candidate, THEN THE SYSTEM SHALL reject the recovery without changing the
Attempt.
Test: src/tester_harness_recovery.rs (request_rejects_dirty_candidate_without_mutating_state)
Test: src/tester_harness_recovery.rs (request_rejects_drifted_candidate_without_mutating_state)

### B10i

WHEN an operator repeats the same accepted repair request, THE SYSTEM SHALL
reuse the existing repair Writer.
Test: src/tester_harness_recovery.rs (repeated_request_reuses_existing_repair_writer)

### B10j

WHEN an operator repeats the same scheduler resume, THE SYSTEM SHALL reuse the
existing resume dispatch.
Test: tests/binary.rs (tester_harness_repair_public_workflow_is_audited_and_idempotent)

### B10k

WHEN an eligible suspended Attempt has no active dispatch and its latest
dispatch is canceled, THE SYSTEM SHALL append a fresh resume bound to the exact
current Attempt without reviving or linking the canceled dispatch.
Test: src/queue.rs (resume_after_canceled_history_uses_the_current_exact_attempt)

### B10l

WHEN Fluent launches a tracked Tester-harness repair Writer, THE SYSTEM SHALL
provide the operator's recorded repair decision as its correction authority and
the failed Tester artifacts for diagnosis without turning ordinary failed Tester
commands into correction obligations for that narrowly scoped repair.
Test: src/work_task_executor.rs (tester_harness_repair_matrix_excludes_and_sanitizes_failed_tester_commands)
Test: src/work_task_executor.rs (tester_harness_repair_execution_context_keeps_artifact_without_tester_findings)
Test: src/work_task_executor.rs (reviewed_repair_prompt_selects_scoped_execution_context_policy)

### B10m

IF a Tester-harness repair Writer commits changes outside its approved file
boundary, THEN THE SYSTEM SHALL retain an artifact naming the rejected commit,
and restore the exact clean approved candidate.
Test: src/work_task_executor.rs (tester_harness_repair_start_restores_rejected_descendant_for_retry)
Test: src/work_task_executor.rs (tester_harness_repair_writer_restores_rejection_and_retries_through_real_executor)

### B10n

WHEN an operator resumes a rejected Tester-harness repair, THE SYSTEM SHALL
retry the same repair Writer from its approved candidate.
Test: src/queue.rs (resume_preserves_history_binds_exact_attempt_and_is_idempotent)
Test: src/work_task_executor.rs (tester_harness_repair_writer_restores_rejection_and_retries_through_real_executor)

### B10o

IF a resumed Tester-harness repair candidate is dirty or is not descended from
its approved candidate, THEN THE SYSTEM SHALL reject the retry without cleaning
or moving the candidate.
Test: src/work_task_executor.rs (tester_harness_repair_start_restores_rejected_descendant_for_retry)

### B11

IF `.fluent/extract-tester-results` exits non-zero or emits output
that does not match the expected schema,
THEN the Tester subcommand SHALL produce `tester-results.json` with
a top-level `error` field carrying the script's exit code and stderr
tail, the captured `commands` array, an empty `tests` array, and a
zero summary. The Tester Task SHALL complete successfully.
Test: src/tester.rs (tester_soft_fails_when_extractor_exits_nonzero)
Test: src/tester.rs (tester_soft_fails_when_extractor_emits_invalid_schema)

### B12

WHEN a writer Task runs in a candidate workspace where
`.fluent/tester.yaml` does not exist,
THE SYSTEM SHALL include in the writer's prompt an instruction to
author `.fluent/tester.yaml` and commit it alongside the rest of
the Work Item's changes.
Test: src/work_task_executor.rs (writer_prompt_includes_tester_yaml_bootstrap_when_missing)

### B13

WHEN a writer Task runs in a candidate workspace where
`.fluent/extract-tester-results` does not exist,
THE SYSTEM SHALL include in the writer's prompt an instruction to
author `.fluent/extract-tester-results` and commit it alongside the
rest of the Work Item's changes.
Test: src/work_task_executor.rs (writer_prompt_includes_extract_tester_results_bootstrap_when_missing)

### B14

WHEN both `.fluent/tester.yaml` and `.fluent/extract-tester-results`
already exist in the candidate workspace at the start of a writer Task,
THE SYSTEM SHALL NOT include the bootstrap instructions in the
writer's prompt.
Test: src/work_task_executor.rs (writer_prompt_omits_bootstrap_when_both_files_present)

### B15

WHEN `tester-results.json` is written,
THE SYSTEM SHALL include exactly these top-level fields: `commands`
(array), `tests` (array), `summary` (object), `error` (object or
null). A standalone Tester result SHALL omit `candidate_commit`; a Work Task
Tester result SHALL add it only through the host binding described below.
Test: src/tester.rs (tester_results_top_level_shape)

Each entry in `commands` SHALL have: `command` (string, as declared
in `tester.yaml`), `exit_code` (integer), `duration_ms` (integer),
`stdout_log` (string, relative path from the artifact directory),
`stderr_log` (string, relative path).
Test: src/tester.rs (tester_results_command_entry_shape)

WHEN a command enables `reject_process_leaks` and Fluent records
process-settlement evidence,
THE SYSTEM SHALL include the Fluent-created settlement directory and every
surviving scoped process record, each identifying its PID, process kind, and
settlement directory, without replacing the command's stdout, stderr, exit
code, or duration.
Test: src/tester.rs (tester_host_process_settlement_reports_scoped_leak)

Each entry in `tests` SHALL have: `id` (string), `test_harness`
(string identifying the source command), `status` (one of `"pass"`,
`"fail"`, `"skipped"`, `"not_run"`), `duration_ms` (integer or
null), `failure_excerpt` (string or null, at most 500 characters).
Test: src/tester.rs (tester_results_test_entry_shape)
Test: src/tester.rs (tester_results_failure_excerpt_capped_at_500_chars)

`summary` SHALL have: `total`, `pass`, `fail`, `skipped` (all
integers). Counts SHALL equal the corresponding partition of the
`tests` array.
Test: src/tester.rs (tester_results_summary_counts_match_tests_partition)

### B16

WHEN the Tester subcommand runs each declared command,
THE SYSTEM SHALL write the command's complete stdout to
`commands/<n>-stdout.log` and its complete stderr to
`commands/<n>-stderr.log` in the Task's artifact directory, where
`<n>` is the zero-based index of the command in `tester.yaml`'s
declaration order. No truncation.
Test: src/tester.rs (tester_writes_complete_command_logs_to_artifact_dir)

The `tests` array SHALL be ordered by `(test_harness, id)`.
Test: src/tester.rs (tester_results_tests_ordered_by_test_harness_then_id)

### B17

WHEN the `error` field is non-null,
IT SHALL have: `kind` (one of `"tester_yaml_problem"`,
`"extractor_missing"`, `"extractor_failure"`,
`"duplicate_test_ids"`, `"process_settlement_infrastructure"`), `message`
(string), `details` (string).
Test: src/tester.rs (tester_results_error_object_shape)

### B18

WHEN Fluent schedules reviewer Tasks for a code-producing Attempt round,
THE SYSTEM SHALL include the completed Writer transcript as advisory evidence.
Test: src/work_model.rs (each_code_reviewer_receives_writer_transcript_in_input_artifacts)

WHEN Fluent prompts a code reviewer before the final Tester,
THE SYSTEM SHALL identify the Writer's focused verification as advisory and
state that the canonical suite has not run.
Test: src/work_task_executor.rs (work_review_prompt_renders_role_conditional_blocks)

WHEN a concrete missing result prevents the tests reviewer from deciding,
THE SYSTEM SHALL permit that reviewer to run one named harness-native check
without permitting any reviewer to rerun the complete suite.
Test: src/work_task_executor.rs (work_review_prompt_renders_role_conditional_blocks)

WHEN Fluent prompts a behaviors reviewer before the final Tester,
THE SYSTEM SHALL instruct it to verify that every `Test:` reference names a real
test that directly exercises the behavior.
Test: src/work_task_executor.rs (work_review_prompt_renders_role_conditional_blocks)

`prompts/review-behaviors.md` SHALL include explicit instruction to
interpret test failures: distinguish *real* failures (introduced by
the candidate's changes), *infrastructure* failures (environment,
network, flaky dependencies), and *pre-existing baseline* failures
(failed on main before this Attempt). The reviewer's verdict SHALL
reflect this interpretation.
Test: src/work_model.rs (review_behaviors_prompt_describes_failure_interpretation)

### B19

WHEN the `error` field in `tester-results.json` is non-null,
`prompts/review-behaviors.md` SHALL instruct the reviewer to produce
a `fail` verdict that names the error `kind` and `message`.
Test: src/work_model.rs (review_behaviors_prompt_handles_error_field)

The Tester subcommand SHALL be reachable through the existing
`fluent` binary's CLI surface.
Test: tests/binary.rs (fluent_help_lists_tester_subcommand)

### B20

WHEN the Tester subcommand executes a Task,
THE SYSTEM SHALL NOT spawn any Coder process for that Task. No
transcript file SHALL be written by Tester (the existing reviewer
and writer transcript paths are unaffected).
Test: src/work_task_executor.rs (tester_task_does_not_spawn_coder_process)
Test: src/work_task_executor.rs (tester_task_does_not_write_transcript)

### B21

WHEN the Tester assembles `tester-results.json` from the extractor
output and two or more test entries share the same `id`,
THE SYSTEM SHALL produce `tester-results.json` with
`error.kind == "duplicate_test_ids"`, name the colliding ids in the
error, and emit no pass/fail partition.
Test: src/tester.rs (duplicate_test_ids_produce_error)

### B22

WHEN the writer bootstraps `.fluent/extract-tester-results` from the
contract in `prompts/write-user.md` because the file is missing,
THE SYSTEM SHALL instruct that every emitted test `id` be globally
unique across all commands and harnesses, prefixing with the binary,
file, or section when a test name recurs.
Test: src/work_task_executor.rs (extract_tester_results_bootstrap_requires_unique_ids)

### B23

WHEN a Work Task Tester produces normalized evidence, THE SYSTEM SHALL bind the
result atomically and idempotently to that Task's exact candidate commit before
reviewers can consume it, and SHALL fail closed if evidence is already bound to
a different commit.
Test: src/tester.rs (tester_results_bind_once_to_the_exact_candidate_commit)

### B24

IF a code-producing final Tester reports an introduced failure, a top-level
infrastructure error, or no readable `tester-results.json`,
THEN THE SYSTEM SHALL block Learning and Merge Candidate readiness and return
established unresolved candidate defects to a corrective Writer while write-round
budget remains. Missing or uninterpretable evidence SHALL pause for diagnosis
without authorizing source correction.
Test: src/work_attempt_loop.rs (tester_failure_routes_to_followup_within_budget)
Test: src/work_attempt_loop.rs (tester_error_routes_to_followup_within_budget)
Test: src/work_attempt_loop.rs (unreadable_final_tester_results_fail_closed)

### B25

WHEN the final Tester completes with no introduced failure and no infrastructure
error,
THE SYSTEM SHALL preserve the passing review decision and advance the exact
reviewed candidate to Learning.
Test: src/work_attempt_loop.rs (passing_final_tester_advances_the_exact_reviewed_candidate)

### B26

WHEN Fluent schedules a code reviewer before the final Tester,
THE SYSTEM SHALL omit `tester-results.json` from that reviewer's input artifacts.
Test: src/work_model.rs (code_review_tasks_do_not_receive_tester_results)

### B27

WHEN an Attempt starts its initial Writer,
THE SYSTEM SHALL capture the pre-write Tester baseline before candidate changes.
Test: src/work_task_executor.rs (capture_baseline_tester_persists_results_as_artifact)

### B28

WHEN the same Attempt starts a corrective Writer,
THE SYSTEM SHALL reuse the original pre-write Tester baseline without running it
again, even when no final Tester Task exists yet.
Test: src/work_task_executor.rs (corrective_writer_does_not_recapture_attempt_baseline_without_tester)

### B29

WHEN scheduler-launched Attempts have the same source commit, Tester
configuration, result extractor, Fluent build, platform, architecture, and
sandbox mode, THE SYSTEM SHALL run one pre-write Tester baseline and give every
Attempt a complete local copy with digest-bearing provenance for that immutable
evidence.
Test: src/scheduler.rs (scheduler_child_receives_one_shared_baseline_session_without_disabling_sandbox)
Test: src/baseline_cache.rs (matching_identity_reuses_one_trustworthy_run)
Test: src/baseline_cache.rs (concurrent_matching_requests_single_flight)

### B30

WHEN any shared-baseline identity field differs or the scheduler restarts,
THE SYSTEM SHALL capture a new baseline instead of reusing earlier evidence.
Test: src/baseline_cache.rs (each_identity_field_invalidates_reuse)

### B31

IF a pre-write Tester returns a harness error, THE SYSTEM SHALL retain that
Attempt-local evidence and SHALL NOT publish it for another Attempt. IF a
published artifact is changed, THEN THE SYSTEM SHALL fail closed before reusing
it and SHALL retain every previously copied Attempt artifact independently.
Test: src/baseline_cache.rs (harness_error_stays_attempt_local_and_is_not_cached)
Test: src/baseline_cache.rs (tampered_shared_artifact_fails_closed_without_rerun)

### B32

WHEN the scheduler launches an Attempt,
THE SYSTEM SHALL retain the ordinary sandbox default for that Attempt.
Test: src/scheduler.rs (scheduler_child_receives_one_shared_baseline_session_without_disabling_sandbox)

### B33

WHEN a scheduler-launched Attempt runs a baseline Tester or final Tester,
THE SYSTEM SHALL remove the scheduler baseline-session token from every project
test command and result extractor.
Test: src/tester.rs (tester_children_do_not_inherit_scheduler_baseline_session)

### B34

WHEN `fluent tester check` runs and its configured commands do not modify
the project,
THE SYSTEM SHALL preserve existing project files and SHALL NOT create
initialization files such as `.fluent/.gitignore` or edit `AGENTS.md`.
Test: tests/behaviors/operations/test-tester-check-source-integrity.sh (tester check preserves project files)

### B35

WHEN a final Tester binds trustworthy evidence to its exact reviewed commit,
THE SYSTEM SHALL preserve enough identity and integrity evidence to reuse that
test run if the same commit later lands, without changing the Tester verdict.
Test: src/baseline_cache.rs (landed_baseline_replaces_next_attempt_capture)

### B36

WHEN a Merge Candidate lands at the exact commit and Tester boundary named by
its final Tester seed,
THE SYSTEM SHALL make that existing evidence the durable landed baseline without
running the Tester again.
Test: src/baseline_cache.rs (landed_baseline_replaces_next_attempt_capture)
Test: src/work_merge_executor.rs (public_land_promotes_final_tester_and_merged_retry_reuses_it)

### B37

WHEN the merged commit or current Tester boundary differs from the final Tester
seed,
THE SYSTEM SHALL NOT reuse the final Tester evidence as the landed baseline.
Test: src/baseline_cache.rs (changed_landed_commit_refuses_final_tester_promotion)

### B38

WHEN neither exact final Tester nor exact merge-local Tester evidence is reusable
after a successful land,
THE SYSTEM SHALL run one fresh baseline against the exact merged commit without
changing the target checkout.
Test: src/work_merge_executor.rs (landed_baseline_checkout_uses_exact_commit_and_is_removed)

### B39

WHEN a direct or scheduler-launched Attempt matches a durable landed-baseline
identity,
THE SYSTEM SHALL copy and validate that evidence before its initial Writer and
SHALL NOT run another baseline.
Test: src/baseline_cache.rs (landed_baseline_replaces_next_attempt_capture)

### B40

WHEN a scheduler session starts or stops,
THE SYSTEM SHALL preserve durable landed-baseline entries.
Test: src/baseline_cache.rs (scheduler_session_cleanup_preserves_landed_baselines)

### B41

IF final Tester evidence changes after its seed manifest is written, contains a
harness error, or otherwise lacks trustworthy provenance,
THEN THE SYSTEM SHALL NOT promote it as a landed baseline.
Test: src/baseline_cache.rs (changed_final_tester_artifact_fails_before_promotion)
Test: src/baseline_cache.rs (harness_error_stays_attempt_local_and_is_not_cached)

### B42

WHEN a fresh landed baseline reports ordinary test failures,
THE SYSTEM SHALL retain that result as reusable baseline evidence.
Test: src/baseline_cache.rs (landed_capture_publishes_ordinary_test_failures)

### B43

IF baseline preparation fails after a durable fast-forward,
THEN THE SYSTEM SHALL leave the candidate merged and retry preparation when the
same land command is invoked again.
Test: src/work_merge_executor.rs (baseline_preparation_failure_keeps_land_merged_and_retry_recovers)

## Reviewer inspection evidence

### B1

WHEN a Review Task reports `Verdict: pass`, THE SYSTEM SHALL require durable
evidence that binds the Review Task role and exact candidate commit to at least
one tracked candidate file and a nonempty explanation of what the role checked.
Test: src/work_task_executor.rs (passing_review_completes_with_candidate_bound_inspection_artifact)
Test: src/review_inspection.rs (accepts_candidate_bound_file_inspection)

### B2

WHEN Fluent validates evidence for a passing review, THE SYSTEM SHALL accept it
only when one successful inspection identifies both the exact candidate and at
least one declared file, using a joined absolute path, candidate workspace plus
repository-relative path, or exact candidate commit plus repository-relative
path.
Test: src/review_inspection.rs (accepts_recovered_codex_inspection)
Test: src/review_inspection.rs (accepts_successful_codex_git_object_inspection)
Test: src/review_inspection.rs (accepts_successful_codex_commit_object_inspection_without_workspace)
Test: src/review_inspection.rs (accepts_successful_claude_inspection)
Test: src/review_inspection.rs (accepts_successful_claude_split_workspace_and_path_inspection)
Test: src/review_inspection.rs (accepts_successful_claude_split_commit_and_path_inspection)
Test: src/review_inspection.rs (accepts_successful_pi_inspection)
Test: src/review_inspection.rs (accepts_successful_pi_split_workspace_and_path_inspection)

### B3

IF a passing review omits inspection evidence, binds it to the wrong candidate
or role, names an invalid or untracked path, leaves its explanation empty, or
has no successful inspection for a declared file, THEN THE SYSTEM
SHALL reject the Review Task as failed and SHALL NOT accept the passing verdict.
Test: src/review_inspection.rs (rejects_candidate_mismatch)
Test: src/review_inspection.rs (rejects_role_mismatch)
Test: src/review_inspection.rs (rejects_wrong_schema_version)
Test: src/review_inspection.rs (rejects_empty_inspection_list)
Test: src/review_inspection.rs (rejects_missing_empty_or_noncanonical_inspections)
Test: src/review_inspection.rs (rejects_duplicate_inspection_paths)
Test: src/review_inspection.rs (rejects_transcript_without_tool_inspection)
Test: src/review_inspection.rs (rejects_failed_only_codex_inspection)
Test: src/review_inspection.rs (rejects_failed_only_pi_inspection)
Test: src/review_inspection.rs (rejects_partial_wrong_and_cross_event_candidate_evidence)
Test: src/work_task_executor.rs (passing_review_without_successful_inspection_fails_closed)
Test: tests/binary.rs (work_task_run_rejects_passing_review_without_inspection_evidence)

### B4

WHEN an earlier reviewer inspection fails but a later inspection succeeds and
names a declared candidate file, THE SYSTEM SHALL accept otherwise valid
passing inspection evidence.
Test: src/review_inspection.rs (accepts_recovered_codex_inspection)

### B5

WHEN a Review Task reports `Verdict: fail` or `Verdict: uncertain`, THE SYSTEM
SHALL complete the Task without requiring passing-review inspection evidence.
Test: src/work_task_executor.rs (nonpassing_review_does_not_require_inspection_artifact)

## Pre-review completion gate

### B1

WHEN Fluent creates a matrix-backed Attempt with required Plan rows, THE SYSTEM
SHALL materialize an unchecked host-owned required-progress projection before the
first Writer launches.
Test: src/work_attempt_loop.rs (new_attempt_materializes_required_progress_before_writer)
Test: src/work_task_executor.rs (matrix_backed_writer_prompt_keeps_required_progress_host_owned)

### B2

WHEN Writer-owned Plan completion is valid, THE SYSTEM SHALL replace only the
required-progress section with exact immutable Writer-owned Plan text and matrix evidence
references while preserving separate progress sections.
Test: src/work_attempt_loop.rs (complete_matrix_repairs_writer_progress_before_review)
Test: src/work_model.rs (completed_matrix_projects_exact_required_progress)

### B3

IF the immutable Plan progress contract and validated matrix Plan rows disagree,
THEN THE SYSTEM SHALL pause before planning a Tester or reviewer.
Test: src/work_model.rs (required_progress_projection_rejects_matrix_contract_disagreement)
Test: src/work_attempt_loop.rs (matrix_progress_contract_disagreement_pauses_before_review)

### B4

WHEN an Attempt has no required-progress contract, THE SYSTEM SHALL preserve the
transition from a completed Writer to review planning.
Test: src/work_attempt_loop.rs (legacy_completed_writer_plans_reviews_without_progress_contract)

### B5

WHEN Fluent retries the completed-Writer planning boundary after its transition
is durable, THE SYSTEM SHALL reuse the existing Writer continuation without
creating a duplicate.
Test: src/work_attempt_loop.rs (post_write_gate_retry_does_not_duplicate_continuation)

### B6

WHEN Fluent retries the completed-Writer planning boundary after review Tasks
are durable, THE SYSTEM SHALL reuse those Tasks without creating
duplicates.
Test: src/work_attempt_loop.rs (post_write_gate_retry_does_not_duplicate_review_tasks)

### B7

WHEN completion evidence remains incomplete and the completed Writer did not
create a candidate commit, THE SYSTEM SHALL preserve the candidate and pause the
Attempt before planning another Writer, Tester, or reviewer.
Test: src/work_attempt_loop.rs (incomplete_no_change_writer_pauses_instead_of_continuing)

### B8

WHEN two consecutive Writer continuations, including after review, create commits
without completing additional work against the same obligations,
THE SYSTEM SHALL preserve the candidate and pause the Attempt before planning a
Writer, Tester, or reviewer.
Test: src/work_attempt_loop.rs (second_stagnant_pre_review_continuation_pauses)
Test: src/work_attempt_loop.rs (stagnant_post_review_continuations_pause)

### B9

WHEN a subsequent pre-review or reviewed-correction Writer uses the same
provider as the preceding Writer and that Writer recorded a session identity
with its required provider-owned material, THE SYSTEM SHALL resume that exact
provider session from the candidate workspace using only options accepted by
the provider's resume command.
Test: src/work_task_executor.rs (writer_route_resumes_the_matching_recorded_provider_session)
Test: src/work_task_executor.rs (reviewed_correction_resumes_the_latest_compatible_writer_session)
Test: src/work_task_executor.rs (completed_codex_writer_snapshot_is_selected_for_reviewed_correction)
Test: src/coder.rs (resumed_writer_codex_command_uses_the_recorded_session)
Test: src/coder.rs (resumed_writer_claude_command_uses_the_recorded_session)
Test: src/codex_worker.rs (writer_session_snapshot_round_trips_without_auth_or_configuration)

### B10

IF a subsequent Writer has no usable session identity or lacks the
provider-owned session material required to resume it, THEN THE SYSTEM SHALL
start a fresh persistent Writer session.
Test: src/work_task_executor.rs (writer_session_falls_back_to_fresh_when_identity_is_unusable)
Test: src/coder.rs (fresh_writer_codex_command_persists_its_session)

### B11

WHEN Fluent prompts a pre-review Writer continuation, THE SYSTEM SHALL provide the
unresolved progress, latest candidate change, and current findings without
repeating completed progress.
Test: src/work_task_executor.rs (continuation_prompt_contains_only_current_execution_context)

### B12

WHEN a pre-review Writer reduces the applicable matrix or legacy
required-progress completion count, THE SYSTEM SHALL preserve the new candidate
commit and pause before planning a Tester or reviewer.
Test: src/work_attempt_loop.rs (regressed_required_progress_pauses_before_review)

### B13

WHEN Fluent prepares a Writer continuation or reviewer, THE SYSTEM SHALL write a
bounded generated context containing the candidate and base commits, changed
files, unresolved steps, deduplicated current findings, passing Tester evidence,
executed commands, and historical artifact paths, and SHALL report omitted item
counts when any bounded collection is truncated.
Test: src/work_task_executor.rs (execution_context_is_bounded_and_reports_omissions)
Test: src/work_task_executor.rs (execution_context_deduplicates_findings_and_summarizes_evidence)

### B14

WHEN generated execution context references historical evidence, THE SYSTEM
SHALL leave each full historical artifact intact and inject its path rather than
its full contents into the continuation or review prompt.
Test: src/work_task_executor.rs (execution_context_is_bounded_and_reports_omissions)
Test: src/work_task_executor.rs (continuation_prompt_contains_only_current_execution_context)

### B15

WHEN Fluent reports status or shows a Work Item, THE SYSTEM SHALL expose review
rounds, completed-stage duration, active duration, provider wait, local transcript
token use, repeated findings, artifact bytes, and pre-review cycles avoided,
deriving measurements only from that Work Item's stored model and project-local
artifacts.
Test: src/work_status.rs (metrics_are_derived_from_local_work_evidence)
Test: src/usage.rs (task_usage_is_persisted_beside_the_transcript)
Test: tests/binary.rs (work_show_outputs_pretty_json_for_one_work_item)

### B16

WHEN Fluent creates a new code-producing Attempt from approved Behaviors,
Approach, and Plan inputs, THE SYSTEM SHALL stamp an immutable Writer completion
contract containing every EARS statement and its existing verification marker,
the complete text of every applicable Approach decision, structural boundary,
and execution-guidance constraint, and every required Plan row with its existing
Verification cell and approved responsibility assignment.
Test: src/work_model.rs (writer_completion_contract_maps_approved_planning_inputs)
Test: src/work_task_executor.rs (writer_route_preserves_wrapped_execution_guidance_in_matrix)
Test: src/work_model.rs (explicit_plan_responsibility_survives_intake_and_rejects_writer_claims)

### B17

WHEN Fluent launches an initial or corrective Writer, THE SYSTEM SHALL
materialize a bounded task-owned `writer-completion.json` matrix before launch,
carry forward completed planning claims from the preceding Writer's authoritative
matrix, including its approved ownership mapping, and add every
currently open review finding as a pending row without inventing test-selector
names. THE SYSTEM SHALL enumerate in the Writer instructions every Behavior,
Approach, and required Plan row ID that a finding may cite.
Test: src/work_task_executor.rs (corrective_writer_matrix_carries_planning_rows_and_adds_open_findings)

WHEN current verification evidence establishes a candidate defect, THE SYSTEM
SHALL add only unresolved candidate-correction obligations to the Writer's work,
preserving applicable accepted proof and its external owner through unrelated
failures and no-change Writer history.
Test: src/work_attempt_loop.rs (writer_receives_only_unresolved_candidate_defects)
Test: src/recovery_contracts.rs (accepted_evidence_survives_unrelated_failures_and_no_change_rounds)
Test: src/work_task_executor.rs (corrective_writer_inherits_approved_ownership_matrix)
Test: src/work_task_executor.rs (continuation_prompt_contains_only_current_execution_context)
Test: src/work_task_executor.rs (reviewed_correction_prompt_is_bounded_and_references_full_artifacts_by_path)
Test: src/work_model.rs (writer_completion_matrix_refuses_unbounded_context)

WHEN Fluent assigns a corrective Writer finding, THE SYSTEM SHALL bind its
finding identity, requirement text, source candidate, and source evidence to
the host-owned Writer Task before launch.
Test: tests/binary.rs (work_attempt_run_plans_followup_for_failed_reviews)

WHEN a corrective Writer produces a later candidate, THE SYSTEM SHALL keep its
host-owned assignment unchanged while evaluating the later candidate's current
verification state.
Test: tests/binary.rs (corrective_writer_assignment_survives_candidate_change)

### B18

WHEN a completed Writer leaves a Writer-owned obligation pending or without
required implementation evidence, passing focused verification, or applicable
finding constraints, and the no-output, regression, stagnation, and structural
pause rules permit continuation, THE SYSTEM SHALL plan one Writer continuation
before any ordinary reviewer or final Tester. Pending host and workflow rows
follow Host-action ownership:B1–B3 and their existing stage gates.
Test: src/work_attempt_loop.rs (incomplete_writer_completion_matrix_returns_to_writer_before_review)
Test: src/work_model.rs (writer_completion_matrix_reconciles_claims_and_corrective_findings)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (completion matrix blocks review and final Tester)
Test: src/work_attempt_loop.rs (writer_completion_excludes_host_and_later_stage_work)
Test: src/work_attempt_loop.rs (pending_host_action_pauses_before_another_writer)

WHEN a corrective Writer produces a new candidate, THE SYSTEM SHALL validate
its completion matrix against the same assigned finding identities and source
bindings while keeping the new candidate's verification requirements separate.
Test: tests/binary.rs (corrective_writer_assignment_survives_candidate_change)

### B19

IF a Writer completion matrix is missing, malformed, contradictory, or changes
an immutable row field, THEN THE SYSTEM SHALL pause with an actionable diagnosis
before any reviewer or final Tester.
Test: src/work_attempt_loop.rs (missing_writer_completion_matrix_pauses_before_review)
Test: src/work_attempt_loop.rs (rewritten_writer_completion_matrix_pauses_before_review)
Test: src/work_model.rs (writer_completion_matrix_reconciles_claims_and_corrective_findings)

IF a corrective Writer assignment cannot be established from trusted host
records, or its binding evidence is changed or contradictory, THEN THE SYSTEM
SHALL pause at `needs-user` with a specific diagnosis before downstream work.
Test: tests/binary.rs (corrective_writer_assignment_rejects_invalid_authority)

## Public workflow continuation

### B1

IF an in-scope failure leaves Work unfinished, THEN THE SYSTEM SHALL provide a
supported public route to continue that Work from its retained state once
required access, evidence, and authorization are supplied.
Test: tests/binary.rs (writer_completion_recovery_committed_matrix_public_continuation)
Test: tests/binary.rs (writer_completion_recovery_later_committed_matrix_public_continuation)
Test: tests/binary.rs (writer_completion_plan_public_continuation)
Test: tests/binary.rs (writer_completion_plan_existing_approval_continues)
Test: tests/binary.rs (writer_completion_plan_replays_same_successor)

For a completed Writer with a committed candidate and malformed completion
matrix, the public repair command records explicit authorization and schedules
one corrective Writer. A separate resume executes it. The source Task, matrix,
transcript, and candidate history remain intact; changed bindings and live
owners reject stale recovery. Host checks, review, final Tester, and Learner
retain their existing gates.

When a later completed Writer in the same Attempt has a malformed matrix, the
earlier completed recovery remains history. The public command requires a new
decision for the later source and records a separate audit and corrective Task.
Repeating the later decision reuses its identities; changed source evidence,
candidate, assignment, ownership, or cancellation blocks it. Status names the
current repair only while that repair is eligible.

### B20

WHEN every Writer-owned obligation and applicable finding is satisfied, the
host-owned required-progress projection and deterministic gates reconcile, and
required host checks have passing evidence accepted by targeted Tests Review,
THE SYSTEM SHALL attach the matrix to every ordinary reviewer as an advisory
coverage map and plan those reviewers without planning the final Tester yet.
Test: src/work_attempt_loop.rs (complete_writer_completion_matrix_reaches_review_and_is_attached)
Test: src/work_task_executor.rs (work_review_prompt_renders_role_conditional_blocks)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (completion matrix admits review and final Tester)
Test: src/work_attempt_loop.rs (pending_host_action_pauses_before_another_writer)
Test: src/host_evidence.rs (pending_host_verification_blocks_readiness)

WHEN Fluent evaluates a corrective Writer's completion, THE SYSTEM SHALL keep
its assigned findings as part of that completion check.
Test: tests/binary.rs (corrective_writer_assignment_survives_candidate_change)

WHEN a corrective Writer's assigned findings and the other pre-review gates
pass, THE SYSTEM SHALL admit the corrected candidate to ordinary reviews while
its complete final Tester remains pending.
Test: tests/binary.rs (corrected_candidate_reaches_reviews_before_final_tester)

### B21

WHEN Fluent retries matrix materialization or the completed-Writer admission
boundary, THE SYSTEM SHALL preserve existing Writer claims and SHALL NOT create
a duplicate Writer continuation.
Test: src/work_task_executor.rs (corrective_writer_matrix_carries_planning_rows_and_adds_open_findings)
Test: src/work_attempt_loop.rs (incomplete_writer_completion_retry_does_not_duplicate_continuation)

### B21a

WHEN an audited host transition revises a Writer completion matrix, THE SYSTEM
SHALL use that matrix as the source for the next Writer and carry its completed
planning claims into later Writers.
Test: src/work_task_executor.rs (corrective_writer_inherits_audited_reference_revision_matrix)

### B21b

WHEN Fluent retries a Writer whose pending completion matrix descends through
stale continuation matrices from an audited host revision, THE SYSTEM SHALL add
the revision's missing exactly matching planning claims without replacing
current Writer claims or review findings.
Test: src/work_task_executor.rs (corrective_writer_inherits_audited_reference_revision_matrix)

### B22

WHEN an older Attempt has no Writer completion contract, THE SYSTEM SHALL retain
its Writer-owned required-progress validation and review-planning behavior,
including rejection of malformed or rewritten required rows.
Test: src/work_attempt_loop.rs (legacy_completed_writer_plans_reviews_without_progress_contract)
Test: src/work_attempt_loop.rs (unchecked_required_progress_plans_writer_before_tester_or_review)
Test: src/work_attempt_loop.rs (malformed_required_progress_pauses_before_tester_or_review)
Test: src/work_attempt_loop.rs (rewritten_required_progress_pauses_before_tester_or_review)

### B23

WHEN a completed Writer records both a base commit and candidate commit, THE
SYSTEM SHALL run deterministic candidate checks before completion-matrix
admission or model review and persist `candidate-gate.json` under the Attempt's
gate artifacts.
Test: src/work_attempt_loop.rs (passing_candidate_gate_is_attached_to_every_reviewer)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (candidate gate blocks review and final Tester)

### B24

WHEN a Writer candidate passes deterministic checks, THE SYSTEM SHALL attach
the exact candidate's gate evidence to every planned reviewer.
Test: src/work_attempt_loop.rs (passing_candidate_gate_is_attached_to_every_reviewer)

### B25

WHEN deterministic candidate checks run, THE SYSTEM SHALL reject a candidate
whose whitespace, linear history, author or committer identity, or subject
does not satisfy the cross-project integrity rules.
Test: src/candidate_gate.rs (literal_backslash_n_does_not_mask_whitespace_or_project_failure)
Test: src/candidate_gate.rs (literal_backslash_n_does_not_mask_blocking_metadata)
Test: src/candidate_gate.rs (rejects_merge_commits)
Test: src/candidate_gate.rs (rejects_invalid_identities)

### B25aa

WHEN structurally valid commit metadata differs only in body presence, body
line wrapping, literal `\n` text, or attribution-trailer style, THE SYSTEM SHALL
pass the built-in commit-metadata check and leave presentation policy to the
project's check hook.
Test: src/candidate_gate.rs (accepts_subject_only_and_coauthored_commits_as_project_policy)
Test: src/candidate_gate.rs (accepts_overlong_commit_body_lines_as_project_policy)
Test: src/candidate_gate.rs (accepts_literal_backslash_n_as_project_policy)

### B25ab

WHEN a structurally valid candidate differs from Factory's commit-message
guidance only because its commit messages lack bodies, contain body lines
longer than 72 characters, or contain literal `\n` text, THE SYSTEM SHALL allow
the project check to pass and may report those presentation issues as advice.
Test: tests/behaviors/operations/test-commit-message-policy.sh (commit-message presentation is advisory)
Test: tests/behaviors/operations/test-commit-message-policy.sh (literal backslash-n text is advisory)

### B25ac

IF a candidate violates commit-history or identity integrity, has an empty
subject, contains a `Co-Authored-By` trailer, or fails another project check,
THEN THE SYSTEM SHALL continue to reject or block that candidate even when its
message also contains literal `\n` text.
Test: tests/behaviors/operations/test-commit-message-policy.sh (provenance and non-presentation failures remain blocking)
Test: tests/behaviors/operations/test-commit-message-policy.sh (literal backslash-n advice does not mask provenance failure)
Test: src/candidate_gate.rs (literal_backslash_n_does_not_mask_whitespace_or_project_failure)
Test: src/candidate_gate.rs (literal_backslash_n_does_not_mask_blocking_metadata)
Test: src/candidate_gate.rs (rejects_merge_commits)
Test: src/candidate_gate.rs (rejects_invalid_identities)

### B25ad

WHEN literal `\n` text is the only concern for a completed Writer candidate,
THE SYSTEM SHALL send that same candidate to review without planning a
corrective Writer, authorizing a commit-message rewrite, or pausing the Attempt.
Test: src/work_attempt_loop.rs (literal_backslash_n_candidate_advances_without_corrective_writer)

### B25a

WHEN deterministic candidate checks run, THE SYSTEM SHALL require the workspace
to have the exact candidate at HEAD and be clean before review.
Test: src/candidate_gate.rs (mismatched_candidate_head_blocks_with_persisted_identity_evidence)
Test: src/candidate_gate.rs (dirty_candidate_workspace_blocks_before_review)

### B25b

IF a candidate has no admitted metadata rewrite, THEN THE SYSTEM SHALL require
the task base to be an ancestor of the candidate before review.
Test: src/candidate_gate.rs (blocks_non_ancestor_candidate_without_prior_gate)

### B25c

WHEN Fluent first rejects a Writer candidate, THE SYSTEM SHALL add its gate
exactly once to the Attempt artifact index before planning a corrective Writer
with that same canonical reference.
Test: src/work_attempt_loop.rs (first_rejected_candidate_gate_is_indexed_before_correction)
Test: tests/binary.rs (first_rejected_candidate_gate_persists_source_binding)

### B25ca

WHEN a corrective Writer returns a candidate after a rejected candidate gate,
THE SYSTEM SHALL re-run deterministic checks across the complete candidate
range from the original base so a later commit cannot hide an earlier defect.
Test: src/candidate_gate.rs (continuation_rechecks_the_complete_candidate_range)
Test: src/candidate_gate.rs (appended_commit_cannot_hide_prior_metadata_failure)

### B25d

IF the only prior candidate-gate failure was commit metadata (including
historical literal `\n` failures) and a corrective Writer replaces the rejected
history, THEN THE SYSTEM SHALL admit the replacement only when both candidates
descend from the same original base and have identical trees.
Test: src/candidate_gate.rs (accepts_tree_identical_metadata_rewrite_from_legacy_gate)
Test: src/candidate_gate.rs (blocks_metadata_rewrite_when_the_candidate_tree_changes)
Test: src/candidate_gate.rs (blocks_history_rewrite_after_mixed_gate_failures)
Test: src/work_attempt_loop.rs (metadata_only_history_repair_reaches_review)

### B25e

WHEN Fluent admits a tree-identical commit-metadata rewrite, THE SYSTEM SHALL
persist the original base, rejected candidate, replacement candidate, shared
tree, and prior gate path in the replacement gate evidence.
Test: src/candidate_gate.rs (accepts_tree_identical_metadata_rewrite_from_legacy_gate)
Test: src/work_attempt_loop.rs (metadata_only_history_repair_reaches_review)

### B25ea

WHEN Fluent admits a tree-identical commit-metadata rewrite, THE SYSTEM SHALL
require the replacement to preserve the protected-base suffix's linear shape,
per-commit trees, and author identities; only commit messages and committer
metadata may change.
Test: src/candidate_gate.rs (blocks_metadata_rewrite_when_commit_author_changes)
Test: src/candidate_gate.rs (blocks_metadata_rewrite_when_history_shape_changes)

### B25f

WHEN Fluent authorizes a tree-identical commit-metadata rewrite, THE SYSTEM
SHALL provide the corrective Writer with the exact commit that must remain the
replacement candidate's protected ancestor.
Test: src/work_task_executor.rs (metadata_only_gate_explicitly_authorizes_tree_identical_rewrite_guidance)

### B25fa

WHEN an operator approves a rejected candidate whose only failed gate check is
commit metadata, THE SYSTEM SHALL record the exact candidate, gate digest,
protected base, candidate tree, and human decision before scheduling one
corrective Writer.
Test: src/candidate_metadata_recovery.rs (request_records_authority_and_is_idempotent)

### B25fb

WHEN a corrective Writer receives older candidate-gate artifacts alongside the
current rejected metadata gate, THE SYSTEM SHALL use the gate naming the
current candidate to select tree-identical metadata-rewrite guidance.
Test: src/work_task_executor.rs (latest_metadata_gate_wins_when_older_gate_history_is_preserved)

### B25fc

WHEN a Writer-completion pause contains one pending Documentation finding for
an historical commit-body line-length defect, and the latest Writer has a
verified no-change result, THE SYSTEM SHALL let the operator use the public
`repair-candidate-metadata` operation to authorize one tree-identical metadata
repair Writer. The operation SHALL bind the review, finding, current
candidate, passing Writer gate, protected parent, candidate tree, and human
decision before changing the Attempt, and SHALL return the same Writer when
the exact authorization is repeated.
Test: src/candidate_metadata_recovery.rs (documentation_review_metadata_recovery_is_public_and_idempotent)
Test: src/candidate_metadata_recovery.rs (documentation_review_metadata_parser_ignores_candidate_range_endpoint)
Test: src/candidate_metadata_recovery.rs (documentation_review_metadata_recovery_rejects_changed_review)

### B25g

WHEN the operator approves candidate-ancestry repair for an authorized
tree-identical metadata rewrite that replaced its protected base, THE SYSTEM
SHALL reconstruct the unchanged candidate tree on the protected base.
Test: src/candidate_ancestry_recovery.rs (repairs_exact_tree_on_protected_base_and_is_idempotent)

### B25h

WHEN Fluent repairs candidate ancestry, THE SYSTEM SHALL preserve the rejected
candidate history.
Test: src/candidate_ancestry_recovery.rs (repairs_exact_tree_on_protected_base_and_is_idempotent)

### B25i

WHEN Fluent repairs candidate ancestry, THE SYSTEM SHALL resume the same Attempt
through its ordinary candidate gate and review lifecycle.
Test: src/candidate_ancestry_recovery.rs (repairs_exact_tree_on_protected_base_and_is_idempotent)
Test: src/candidate_ancestry_recovery.rs (supersedes_interrupted_fresh_queue_admission_and_resumes_exact_repair)

### B25j

IF the candidate worktree is dirty or its HEAD, tree, protected base, rejected
candidate, or gate evidence has drifted, THEN THE SYSTEM SHALL reject the
candidate-ancestry repair without changing Attempt state or candidate history.
Test: src/candidate_ancestry_recovery.rs (rejects_dirty_candidate_without_mutating_history_or_state)
Test: src/candidate_ancestry_recovery.rs (rejects_clean_candidate_head_drift_without_mutating_attempt_state)

### B25k

WHEN an operator repeats a completed candidate-ancestry repair request, THE
SYSTEM SHALL return the recorded repair without creating another recovered
candidate or recovery record.
Test: src/candidate_ancestry_recovery.rs (repairs_exact_tree_on_protected_base_and_is_idempotent)

### B25l

WHEN an interrupted candidate-ancestry repair is retried after its durable
recovery record was written, THE SYSTEM SHALL reconcile either the original or
already-reconstructed candidate into the same recorded Attempt transition.
Test: src/candidate_ancestry_recovery.rs (reconciles_interrupted_repair_after_branch_move)

### B25m

WHEN Fluent plans reviewers for a host-owned candidate-ancestry repair, THE
SYSTEM SHALL provide its durable recovery evidence without requiring a model
Writer transcript.
Test: src/candidate_ancestry_recovery.rs (repairs_exact_tree_on_protected_base_and_is_idempotent)

### B25n

IF unstarted reviewers from a completed candidate-ancestry repair reference an
absent model transcript, THEN repeating the public repair command SHALL replace
that reference with the durable host recovery evidence.
Test: src/candidate_ancestry_recovery.rs (repeated_repair_reconciles_planned_review_inputs_without_fake_transcript)

### B25o

IF obsolete recovery guidance admitted a fresh Attempt after a completed
candidate-ancestry repair, THEN repeating the public repair command SHALL
supersede only an interrupted, output-free initial Writer, preserve an audited
receipt, and authorize an exact resume of the repaired Attempt.
Test: src/candidate_ancestry_recovery.rs (supersedes_interrupted_fresh_queue_admission_and_resumes_exact_repair)

### B25oa

IF a superseded queue admission's initially output-free Writer settles after
its audited supersession, THEN THE SYSTEM SHALL retain that output as history,
keep it ineligible as the recovered candidate, and allow a later ancestry
repair to carry the validated supersession receipt forward.
Test: src/candidate_ancestry_recovery.rs (repairs_a_later_tree_identical_rewrite_after_an_earlier_recovery)

### B25p

WHEN the latest queue dispatch is bound to an earlier recovered Attempt, THE
SYSTEM SHALL display that bound Attempt as the Work Item's current action while
retaining later superseded Attempts in history.
Test: src/work_status.rs (status_uses_the_attempt_bound_to_the_latest_dispatch)

### B25q

IF a later Writer performs another tree-identical history rewrite after an
ancestry repair, THEN the public repair command SHALL create a new recovery only
when the later substantive base descends from the earlier recovered commit and
all other candidate checks pass.
Test: src/candidate_ancestry_recovery.rs (repairs_a_later_tree_identical_rewrite_after_an_earlier_recovery)

### B25r

WHEN Fluent reconstructs a candidate after an ancestry failure, THE SYSTEM
SHALL retain the metadata-valid source message and instruct subsequent Writers
and Reviewers not to request metadata rewrites at or before the latest protected
base.
Test: src/candidate_ancestry_recovery.rs (repairs_a_later_tree_identical_rewrite_after_an_earlier_recovery)
Test: src/work_task_executor.rs (recovered_ancestry_prompts_protect_immutable_history)

### B25s

WHEN an older Attempt paused solely because Factory's former project hook
rejected missing commit bodies or long body lines, THE SYSTEM SHALL recover
only when the source Writer's base predates the advisory policy marker, the
preceding candidate and stopped output-free Writer remain exact, the Writer's
inputs equal the Work Item's preserved inputs plus its exact canonical gate from
the immediately preceding Writer, and the gate and canonical hook log exactly
match the stored legacy evidence. The correction Task's gate input SHALL remain
the source binding when the old first-rejection transition omitted its duplicate
Attempt artifact entry. Fluent SHALL add that missing entry only inside the
audited reclassification transition, preserve the original evidence, supersede
only the corrective Writer, record a separate reclassification receipt, and
continue the same Attempt to review.
Test: src/work_attempt_loop.rs (retired_commit_style_pause_reuses_preceding_candidate)
Test: src/work_attempt_loop.rs (retired_commit_style_pause_preserves_work_item_inputs)
Test: src/work_attempt_loop.rs (retired_commit_style_recovers_legacy_omitted_gate_index)
Test: tests/binary.rs (retired_commit_style_resume_reaches_review_without_writer)
Test: tests/binary.rs (retired_commit_style_legacy_projection_reaches_review_without_writer)

### B25sa

IF a gate-backed corrective Writer's source base already contains the advisory
policy marker, THEN THE SYSTEM SHALL keep foreground and queued resume on the
ordinary corrective-Writer path instead of applying legacy reclassification.
Test: tests/binary.rs (current_candidate_gate_correction_uses_ordinary_resume_paths)

### B25t

IF a retired commit-style pause has a live or output-producing Writer, a dirty
or changed candidate, extra, missing, or duplicate Writer inputs, a
noncanonical or reassigned gate, an ambiguous aggregate gate entry, changed gate
or hook evidence, a prohibited attribution trailer anywhere in the recovered
commit range even when the hook log omits it, or mixed, provenance, or unknown
hook findings, THEN THE SYSTEM SHALL refuse automatic recovery without changing
the Attempt, Task history, candidate, gate, or hook evidence.
Test: src/work_attempt_loop.rs (retired_commit_style_recovery_rejects_unsafe_frontiers)
Test: src/work_attempt_loop.rs (retired_commit_style_legacy_index_recovery_rejects_unsafe_frontiers)
Test: tests/binary.rs (retired_commit_style_resume_rejects_extra_correction_inputs)
Test: tests/binary.rs (retired_commit_style_legacy_projection_rejects_unsafe_frontiers)

### B25u

WHEN foreground or queued execution revisits a completed retired commit-style
reclassification, THE SYSTEM SHALL reuse its recorded candidate transition and
policy commit without rewriting history or duplicating Tasks, including after
an unrelated project-branch advance that retains the recorded policy commit
and current advisory marker and hook.
Test: src/work_attempt_loop.rs (retired_commit_style_admission_survives_unrelated_policy_advance)
Test: src/work_attempt_loop.rs (retired_commit_style_recovery_reuses_matching_unbound_receipt)
Test: tests/binary.rs (retired_commit_style_resume_is_idempotent_across_execution_paths)

### B25v

WHEN Fluent recovers a retired commit-style pause, THE SYSTEM SHALL retain any
completion-contract behavior requirements and still require reviews, the final
Tester, and the Learner before it marks the Merge Candidate ready.
Test: tests/binary.rs (retired_commit_style_resume_is_idempotent_across_execution_paths)

### B26

WHEN deterministic candidate checks run, THE SYSTEM SHALL resolve every
approved `Test:` path and native test identifier without introducing aliases.
Test: src/candidate_gate.rs (rejects_missing_test_paths_and_selectors_without_aliases)
Test: src/candidate_gate.rs (validates_documented_double_colon_test_references)

WHEN a shell reference names a script with a descriptive parenthetical, THE
SYSTEM SHALL check the committed script path without requiring that description
to appear in its source.
Test: src/candidate_gate.rs (matches_shell_script_without_description_literals)

WHEN a `Test:` reference declares a whole-script, named-test, or complete-command
kind, THE SYSTEM SHALL retain that meaning independently of its optional coverage
description and preserve the approved meaning of supported legacy references.
Test: src/candidate_gate.rs (explicit_reference_meaning_controls_evidence_matching)
Test: src/candidate_gate.rs (unambiguous_legacy_reference_meaning_is_preserved)

### B27

WHEN final structured Tester evidence is available, THE SYSTEM SHALL require a
matching passing native identity or bound command occurrence for every approved
`Test:` reference.
Test: src/candidate_gate.rs (validates_test_references_against_passing_structured_tester_evidence)
Test: src/candidate_gate.rs (validates_nextest_integration_test_references)
Test: src/candidate_gate.rs (shell_evidence_uses_the_harness_native_script_identity)

WHEN a reference explicitly names a shell test, THE SYSTEM SHALL require a pass
for its exact script path and native test name. A whole-script pass, another
script's result, or a failed or skipped matching result SHALL NOT satisfy it.
Test: src/candidate_gate.rs (named_shell_reference_requires_exact_script_and_test)
Test: src/candidate_gate.rs (named_shell_reference_rejects_whole_script_and_conflicting_results)

WHEN Rust results unambiguously identify the referenced test, THE SYSTEM SHALL
accept bare or qualified native names, source-path wrappers, and nextest binary
and test-name representations while preserving supplied path, binary, and module
distinctions.
Example: `src/lib.rs (drain_is_idempotent)` can match
`src/lib.rs (tests::lifecycle::drain_is_idempotent)`; a different supplied source
path or conflicting module qualification cannot establish the same identity.
Nextest's `my-crate::bin/tool drain_is_idempotent` also supplies a native binary
identity; its target kind and target name remain part of that identity.
Test: src/candidate_gate.rs (accepts_equivalent_native_rust_identities)

WHEN a shell result uses its script path as its native identity, THE SYSTEM SHALL
match that exact committed path regardless of its harness display label, without
requiring the coverage description in the result.
Test: src/candidate_gate.rs (matches_shell_script_without_description_literals)

IF evidence is failed, skipped, missing, stale, ambiguous, or unrelated, THEN
THE SYSTEM SHALL leave the verification requirement unsatisfied.
Example: A passing qualified Rust test and a failed or skipped qualified test
with the same short name do not disambiguate a short reference. This includes
a passing `my-crate works` alongside a skipped `my-crate::bin/tool works`.
Evidence from another candidate cannot satisfy the current candidate's requirement.
Test: src/candidate_gate.rs (rejects_invalid_or_ambiguous_test_evidence)

WHEN a corrective Writer produces a new candidate, THE SYSTEM SHALL require
fresh verification for that candidate; the source candidate's Tester results
and the Writer's completed correction rows SHALL NOT satisfy its full declared
Tester requirements.
Test: tests/binary.rs (corrected_candidate_requires_fresh_verification)

### B27a

WHEN the final Tester has completed and ordinary baseline comparison would
otherwise pass, THE SYSTEM SHALL validate every approved `Test:` reference
against that exact Tester's passing structured results before creating a Merge
Candidate or launching the Learner.
Test: src/work_attempt_loop.rs (missing_evidence_does_not_authorize_writer)

### B27b

IF required evidence is missing or uninterpretable, THEN THE SYSTEM SHALL route
the requirement to verification recovery without treating that condition alone
as a candidate defect. IF its cause is unknown, THEN THE SYSTEM SHALL pause at
`needs-user` with the requirement, available evidence, and next diagnostic action.
Test: src/work_attempt_loop.rs (missing_evidence_does_not_authorize_writer)
Test: src/work_attempt_loop.rs (unclassified_verification_failure_pauses_for_diagnosis)

### B27c

IF the post-Tester candidate gate rejects frozen-candidate evidence, THEN THE
SYSTEM SHALL NOT create a Merge Candidate or launch the Learner for that
evidence.
Test: src/work_attempt_loop.rs (missing_evidence_does_not_authorize_writer)

### B27d

WHEN Fluent reads an older complete Attempt whose generic Learner failure points
to rejected gate evidence with missing Tester references, THE SYSTEM SHALL
reassess the normalized post-Tester gate in that same Attempt and pause for
diagnosis when evidence alone is missing, preserving the old Learner history.
Test: src/work_attempt_loop.rs (legacy_learner_tester_evidence_failure_pauses_for_diagnosis)

### B25f

WHEN a behavior `Test:` reference is a supported Python command with a direct
project-relative script operand, THE SYSTEM SHALL accept the reference only when
that script path exists in the candidate commit.
Test: src/candidate_gate.rs (python_command_reference_validates_committed_script_operand)
Test: src/candidate_gate.rs (python_command_reference_requires_committed_script_operand)

### B25g

WHEN final Tester evidence is checked for a Python command `Test:` reference,
THE SYSTEM SHALL require either a passing Tester command that exactly matches
the approved complete command or passing candidate-bound host evidence for that
same command accepted by a targeted Tests Review.
Test: src/candidate_gate.rs (python_command_reference_matches_exact_passing_tester_command)
Test: src/candidate_gate.rs (python_command_reference_rejects_different_passing_tester_command)
Test: src/host_evidence.rs (candidate_gate_evidence_is_candidate_bound_reviewed_and_idempotent)
Test: src/candidate_gate.rs (python_command_reference_rejects_unbound_host_command_claim)

### B27e

WHEN Fluent recovers that exact older Attempt, THE
SYSTEM SHALL remove its unreviewed, unlanded, Learner-unapproved Merge Candidate
from pending land.
Test: src/work_attempt_loop.rs (legacy_learner_tester_evidence_failure_pauses_for_diagnosis)

### B27f

IF a generic Learner failure does not carry the exact retained evidence for the
older missing test-reference defect, THEN THE SYSTEM SHALL preserve its
existing Learner recovery disposition.
Test: src/work_attempt_loop.rs (unrelated_generic_learner_failure_keeps_its_existing_recovery)
Test: src/work_attempt_loop.rs (legacy_evidence_recovery_rejects_nonempty_unrecorded_run)

### B27g

WHEN an operator explicitly queues the exact legacy Attempt after an older
false `candidate-ready` or corrected `blocked` dispatch, THE SYSTEM SHALL
revalidate current verification requirements and reject correction admission
when the evidence establishes only a missing-result gap.
Test: src/work_attempt_loop.rs (legacy_evidence_recovery_blocks_false_ready_and_blocked_dispatches)
Test: tests/binary.rs (queue_resume_requires_exact_legacy_learner_evidence)

### B28

WHEN a project provides an executable `check-pre-merge` hook, THE SYSTEM SHALL
run that project-owned check at the candidate gate so formatting and forbidden
dependency checks fail before review and preserve its log.
Test: src/candidate_gate.rs (project_check_failure_rejects_candidate_and_keeps_its_log)
Test: tests/behaviors/operations/test-dashboard-module-boundaries.sh (dashboard module boundary behavior tests passed)

### B29

IF `check-pre-merge` moves HEAD or changes worktree state, THEN THE SYSTEM SHALL
block the gate as invalid host configuration.
Test: src/candidate_gate.rs (a_project_check_that_mutates_the_candidate_blocks_as_invalid_configuration)

### B30

IF deterministic checks establish a Writer candidate defect, THEN THE SYSTEM SHALL return
the structured gate evidence as current findings to one corrective Writer
without planning a reviewer or final Tester. The findings SHALL remain
available when the artifact's absolute path is too long to include in full or
the Writer cannot enumerate its parent directory.
Test: src/work_attempt_loop.rs (deterministic_candidate_failure_returns_to_writer_before_review)
Test: src/work_task_executor.rs (execution_context_surfaces_candidate_gate_diagnostics_from_long_paths)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (candidate gate blocks review and final Tester)

### B31

IF a corrective Writer still fails deterministic checks, or the gate cannot
establish trustworthy Git, Tester, or hook state, THEN THE SYSTEM SHALL pause
with an actionable handoff instead of continuing indefinitely.
Test: src/work_attempt_loop.rs (invalid_project_check_pauses_without_reviewer_or_tester)
Test: src/candidate_gate.rs (malformed_prior_gate_blocks_history_rewrite)
Test: src/candidate_gate.rs (blocks_non_ancestor_candidate_without_prior_gate)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (candidate gate blocks review and final Tester)

### B31a

WHEN the user explicitly resumes a repeated rejected candidate gate, THE SYSTEM
SHALL preserve the same Attempt, candidate workspace, and provider session and
admit exactly one more corrective Writer only when current evidence establishes
an unresolved candidate defect. IF an older binary hard-failed that
already-planned corrective Writer before it produced output, THE SYSTEM SHALL
replan that same Task only when its attached gate evidence still belongs to an
earlier completed Writer; every other failed Attempt remains terminal.
Test: src/work_attempt_loop.rs (failed_candidate_gate_correction_retries_the_same_writer_task)
Test: src/queue.rs (resume_accepts_legacy_failed_candidate_gate_correction_dispatch)

### B32

WHEN a pre-land Learner finishes, THE SYSTEM SHALL run the applicable candidate
checks before Learning succeeds.
Test: src/work_attempt_loop.rs (learner_candidate_gate_rejects_and_rolls_back_bad_expertise_commit)

### B33

IF a capture Learner produces a candidate that fails deterministic checks, THEN
THE SYSTEM SHALL retain the gate evidence, roll back to the reviewed Writer
commit, and keep the Merge Candidate non-ready.
Test: src/work_attempt_loop.rs (learner_candidate_gate_rejects_and_rolls_back_bad_expertise_commit)

## Complete ordinary review coverage

### B1

WHEN an Attempt is ready to advance from review to final Tester or Learning,
THE SYSTEM SHALL require each built-in reviewer role to have a passing ordinary
review for the current candidate or a valid carried-forward ordinary review.
Test: tests/binary.rs (review_coverage_requires_all_roles_after_host_correction)
Test: tests/binary.rs (review_coverage_preserves_valid_ordinary_carry_forward)
Derived: Correction-aware review invalidation:B5 requires fresh affected-role
reviews; it does not establish missing ordinary coverage for other roles.
Example: Behaviors and Skills cannot be omitted just because the last host-only
correction changed no files in their domains.

### B2

WHEN a targeted review passes,
THE SYSTEM SHALL count its verdict only toward its declared evidence scope,
without supplying or replacing ordinary review coverage for that role.
Test: tests/binary.rs (review_coverage_resume_keeps_targeted_and_ordinary_scope_separate)
Derived: Targeted host acceptance and ordinary domain review are distinct
requirements in the existing pre-review and host-evidence contracts.
Example: A passing Plan-step host check does not constitute the ordinary Tests
Review of the implementation. Resuming a saved Attempt must retain that distinction.

## Correction-aware review invalidation

### B1

WHEN a reviewed corrective Writer changes the candidate commit, THE SYSTEM SHALL
compare the immediately preceding reviewed Writer commit with the new candidate
and include additions, changes, deletions, and both sides of copies or renames in
the changed-path set.
Test: src/review_invalidation.rs (changed_paths_include_both_sides_of_a_rename)
Test: src/review_invalidation.rs (changed_paths_include_deletions)

### B2

WHEN every changed path belongs to a known project domain, THE SYSTEM SHALL map
the delta to its documentation, behavior, architecture, skill, and test review
roles and preserve passing roles whose domains remain untouched.
Test: src/review_invalidation.rs (known_paths_invalidate_only_their_review_domains)
Test: src/review_invalidation.rs (rename_paths_are_classified_together)

### B3

WHEN Fluent plans review after a corrective Writer, THE SYSTEM SHALL schedule the
ordered union of reviewers that failed the preceding round, reviewers whose
domains the correction changed, and roles missing ordinary coverage. A pass
SHALL survive only when every intervening Writer delta leaves its domain
unchanged.
Test: src/work_attempt_loop.rs (corrective_review_roles_include_changed_domains_in_default_order)
Test: tests/binary.rs (review_coverage_preserves_valid_ordinary_carry_forward)
Test: tests/behaviors/operations/test-work-attempt-loop.sh (attempt loop plans follow-up with mixed missing review)

### B4

IF a changed path is unknown or unsafe, the prior review context is missing or
mismatched, or Git cannot establish the correction delta, THEN THE SYSTEM SHALL
invalidate every built-in reviewer.
Test: src/review_invalidation.rs (unknown_or_unsafe_paths_invalidate_every_reviewer)
Test: src/review_invalidation.rs (changed_paths_fail_closed_when_git_cannot_compare_commits)
Test: src/work_attempt_loop.rs (ordinary_review_coverage_rejects_ambiguous_prior_context)

### B5

BEFORE Fluent interprets the current review round or advances to final Tester or
Learner, THE SYSTEM SHALL require each built-in role to have a completed passing
ordinary Review Task with the exact current Writer context or valid carry-forward
from its original candidate. It SHALL reopen missing ordinary roles and roles
whose latest ordinary review is mismatched; an unknown role SHALL invalidate all
built-in reviewers. A matching replacement SHALL supersede that role's stale
review without rewriting completed Tasks.
Test: src/work_attempt_loop.rs (exact_candidate_review_check_reopens_only_missing_invalidated_roles)
Test: src/work_attempt_loop.rs (exact_candidate_review_check_reopens_a_mismatched_current_role)
Test: src/work_attempt_loop.rs (exact_candidate_review_check_accepts_the_latest_replacement_for_a_role)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_rejects_mismatched_final_review_context)
Test: src/work_attempt_loop.rs (ordinary_review_coverage_reopens_unknown_review_roles)
Test: tests/binary.rs (review_coverage_resume_keeps_targeted_and_ordinary_scope_separate)

### B6

WHEN a capture Learner changes only `.fluent/expertise/`, THE SYSTEM SHALL keep
pre-Learner review passes valid while enforcing the Learner mutation boundary and
running the deterministic candidate gate against the canonicalized candidate
before Learning succeeds.
Test: src/review_invalidation.rs (learner_owned_expertise_does_not_stale_pre_learner_reviews)
Test: tests/binary.rs (review_coverage_preserves_completed_learner_expertise)
Test: src/work_attempt_loop.rs (learner_path_expertise_subtree_is_in_bounds)
Test: src/work_attempt_loop.rs (learner_path_outside_expertise_is_out_of_bounds)
Test: src/work_attempt_loop.rs (learner_candidate_gate_rejects_and_rolls_back_bad_expertise_commit)

## Reviewed corrective Writer context

### B1

WHEN Fluent prompts a reviewed corrective Writer, THE SYSTEM SHALL write one
current `execution-context.json` and render a prompt no larger than 32 KiB that
references current progress, the completion matrix, planning authority, and
full correction artifacts by managed path rather than embedding their bodies;
derived Work uses a task-owned snapshot of its immutable corrective authority.
Test: src/work_task_executor.rs (reviewed_correction_prompt_is_bounded_and_references_full_artifacts_by_path)
Test: src/work_task_executor.rs (reviewed_correction_references_derived_authority_without_reinjecting_it)

### B2

WHEN Fluent generates corrective context, THE SYSTEM SHALL preserve every
current finding, unresolved progress entry, and correction-input artifact path
within the bounded authoritative core.
Test: src/work_task_executor.rs (execution_context_never_silently_omits_current_findings)
Test: src/work_task_executor.rs (execution_context_is_bounded_and_preserves_authoritative_core)

WHEN Fluent generates corrective context for a corrective Writer, THE SYSTEM
SHALL present the assigned findings from the host-owned Task assignment as
distinct from the current candidate's verification assessment.
Test: tests/binary.rs (corrective_writer_assignment_survives_candidate_change)

WHEN Fluent assesses the current candidate for corrective context, THE SYSTEM
SHALL use that assessment for fresh verification meaning without erasing or
replacing the Writer's assigned source obligations.
Test: tests/binary.rs (corrective_writer_assignment_survives_candidate_change)
Test: tests/binary.rs (corrected_candidate_requires_fresh_verification)

WHEN the candidate gate, Writer context and requirements, or status report
verification, THE SYSTEM SHALL use the same current satisfaction, cause, owner,
and supporting proof. Diagnostic wording and ordering SHALL NOT create or revive
requirements. Changed or expired proof and unknown historical findings SHALL
remain blocking for the applicable advancement.
Test: tests/binary.rs (verification_views_agree_on_current_requirements)
Test: src/recovery_contracts.rs (diagnostic_text_does_not_change_requirements)
Test: src/recovery_contracts.rs (accepted_evidence_survives_unrelated_failures_and_no_change_rounds)

### B3

IF the findings, unresolved progress, and correction-input paths cannot fit
within the execution-context size limit, THEN THE SYSTEM SHALL reject the
context before launch rather than silently omit authoritative work.
Test: src/work_task_executor.rs (execution_context_rejects_authoritative_findings_that_cannot_fit)
Test: src/work_task_executor.rs (execution_context_rejects_artifact_paths_that_cannot_fit)

### B4

WHEN Fluent prepares a reviewed corrective Writer, THE SYSTEM SHALL create a
task-owned `commands/` directory for complete command output while the prompt
directs the Writer to inspect only concise summaries or relevant failure
excerpts in the model conversation.
Test: src/writer_context.rs (final_metrics_separate_cached_replay_from_uncached_input)
Test: src/work_task_executor.rs (reviewed_correction_prompt_is_bounded_and_references_full_artifacts_by_path)

### B5

WHEN a Writer completes, THE SYSTEM SHALL persist prompt kind and bytes,
execution-context and transcript bytes, cached and uncached input tokens, output
tokens, and applicable budget warnings beside the full transcript.
Test: src/writer_context.rs (final_metrics_separate_cached_replay_from_uncached_input)

### B6

IF a reviewed corrective prompt exceeds its host-owned size budget, THEN THE
SYSTEM SHALL reject it before Writer launch.
Test: src/writer_context.rs (reviewed_correction_prompt_budget_is_enforced_before_launch)

## Suite-health gate

### B1

WHEN the first write Task of an Attempt begins and no Tester Task
exists on the Attempt,
THE SYSTEM SHALL run the tester suite on the pre-write workspace and
persist Attempt-local results at
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/<attempt-id>-baseline-tester/tester-results.json`
and, when a scheduler shares the capture, persist its digest-bearing provenance
at the same artifact directory's `baseline-provenance.json`.
Test: src/work_task_executor.rs (capture_baseline_tester_persists_results_as_artifact)
Test: src/baseline_cache.rs (matching_identity_reuses_one_trustworthy_run)

### B2

WHEN the suite-health gate evaluates tester failures and a pre-write
baseline of failing tests exists,
THE SYSTEM SHALL block only on failures the Work Item introduced —
tests failing now that were not failing in the baseline — and SHALL
NOT block on tests already failing at the baseline.
Test: src/work_attempt_loop.rs (preexisting_failures_pass_gate_with_baseline)
Test: src/work_attempt_loop.rs (introduced_failure_blocks_gate_with_baseline)

### B3

WHEN the suite-health gate evaluates tester failures and no baseline
is available,
THE SYSTEM SHALL count all current failures, blocking on any test with
`status == "fail"`.
Test: src/work_attempt_loop.rs (no_baseline_falls_back_to_absolute_count)
Test: src/work_attempt_loop.rs (tester_failure_blocks_merge_candidate)

### B4

WHEN a round is blocked by a Tester failure **or a Tester error** and
the write-round budget remains,
THE SYSTEM SHALL schedule a follow-up write round with the Tester
results as input; when the write-round budget is exhausted, it SHALL
record `needs-user`.
Test: src/work_attempt_loop.rs (tester_failure_routes_to_followup_within_budget)
Test: src/work_attempt_loop.rs (tester_failure_records_needs_user_at_cap)
Test: src/work_attempt_loop.rs (tester_error_routes_to_followup_within_budget)
Test: src/work_attempt_loop.rs (tester_error_records_needs_user_at_cap)

### B5

WHEN the round has a readable `tester-results.json` with **no top-level
error** that reports no introduced Tester failures, OR there is no
readable `tester-results.json`,
THE SYSTEM SHALL leave the round outcome to the reviewer verdicts
unchanged (the gate does not block).
Test: src/work_attempt_loop.rs (passing_or_missing_tester_does_not_block)
Test: src/work_attempt_loop.rs (errored_tester_does_not_fall_through_to_reviewers)

### B6

WHEN the suite-health gate reads a `tester-results.json` whose
top-level `error` is set,
THE SYSTEM SHALL treat the round as not clean and block it, regardless
of the tester baseline.
Test: src/work_attempt_loop.rs (tester_error_blocks_merge_candidate)
Test: src/work_attempt_loop.rs (tester_error_blocks_regardless_of_baseline)

## Update check and nudge

### B1

WHEN fluent runs an update check,
THE SYSTEM SHALL query the fluent.computer update endpoint as its default
release source, sending only its fluent version and OS via User-Agent and
nothing machine-identifying. `FLUENT_API_BASE` still overrides for tests.
Test: tests/binary.rs (update_check_queries_update_endpoint)

### B2

WHEN an update check finds the current version is behind the latest
release,
THE SYSTEM SHALL print to stderr a nudge naming the latest version and
`fluent update`, without changing the invoked command's stdout or exit
status.
Test: tests/behaviors/operations/test-update-nudge.sh

### B3

WHILE running any command other than `fluent update`,
THE SYSTEM SHALL NOT download or replace the binary.
Test: tests/binary.rs (update_check_never_replaces_binary)

### B4

IF the release source cannot be reached during an update check,
THEN THE SYSTEM SHALL continue the command normally, with no error and
no nudge.
Test: tests/binary.rs (update_check_offline_is_silent_and_nonfatal)

### B5

WHEN Fluent would run an update check but one has run within the cached
interval,
THE SYSTEM SHALL NOT query the release source again and SHALL base any
nudge on the cached result.
Test: tests/binary.rs (update_check_is_cached_within_interval)

### B6

WHERE `FLUENT_NO_UPDATE_CHECK` is set,
THE SYSTEM SHALL neither query the release source nor show a nudge.
Test: tests/binary.rs (update_check_env_opt_out_suppresses_check_and_nudge)

## Self-update

### B1

WHEN `fluent update` runs and the current version is already the latest,
THE SYSTEM SHALL report that fluent is up to date and make no changes.
Test: tests/binary.rs (update_reports_up_to_date)

### B2

WHEN `fluent update` runs and a newer release is available,
THE SYSTEM SHALL replace the running binary with the downloaded release.
Test: tests/binary.rs (update_replaces_binary_and_rematerializes_skills)

### B3

WHEN `fluent update` replaces the running binary,
THE SYSTEM SHALL use the updated binary's bare skill command to refresh handoff
data and existing managed installations without creating an absent agent root.
Test: tests/binary.rs (update_replaces_binary_and_rematerializes_skills)
Test: tests/binary.rs (skills_add_bare_does_not_create_agent_roots,
skills_add_bare_updates_existing_project_skill,
skills_add_writes_to_data_directory)

### B4

WHEN `fluent update` replaces the running binary,
THE SYSTEM SHALL report the new version.
Test: tests/binary.rs (update_replaces_binary_and_rematerializes_skills)

### B5

IF `fluent update` cannot reach the release source or the download
fails,
THEN THE SYSTEM SHALL report the failure and leave the current binary
in place.
Test: tests/binary.rs (update_offline_preserves_binary)

### B6

IF replacing the binary does not complete successfully,
THEN THE SYSTEM SHALL leave a working binary in place rather than a
partial or corrupt one.
Test: tests/binary.rs (update_replace_leaves_working_binary_on_failure)

### B7

IF the release omits the matching SHA-256 asset or the downloaded binary does
not match it, THEN THE SYSTEM SHALL reject the update and preserve the installed
binary byte-for-byte.
Test: tests/binary.rs (update_checksum_mismatch_preserves_binary)
Test: tests/binary.rs (update_without_checksum_asset_preserves_binary)

### B8

WHEN the release script runs from a clean commit equal to `origin/main`, the
version tag is unused, and every release gate passes, THE SYSTEM SHALL publish
distinct signed binaries for `aarch64-apple-darwin` and
`x86_64-apple-darwin`, with one matching SHA-256 file for each binary, at that
exact commit.
Test: tests/behaviors/operations/test-release-script.sh (publishes both Mac architectures at exact commit after gates)

### B9

IF the release source is dirty or unsynchronized, the version tag already
exists, a release gate fails, or version-specific release notes are missing,
THEN THE SYSTEM SHALL publish no release.
Test: tests/behaviors/operations/test-release-script.sh (rejects dirty or unsynchronized source)
Test: tests/behaviors/operations/test-release-script.sh (rejects reused tag or failed release gate)
Test: tests/behaviors/operations/test-release-script.sh (rejects missing release notes)

### B10

IF either supported macOS target cannot be built, does not contain its requested
architecture, cannot be signed, or cannot be checksummed, THEN THE SYSTEM SHALL
stop before creating the GitHub release.
Test: tests/behaviors/operations/test-release-script.sh (rejects failed target build before publication)
Test: tests/behaviors/operations/test-release-script.sh (rejects failed target signature before publication)
Test: tests/behaviors/operations/test-release-script.sh (rejects wrong target architecture before publication)
Test: tests/behaviors/operations/test-release-script.sh (rejects failed target checksum before publication)

## Project initialization

### B1

WHEN `fluent init` is invoked at a Git repository root without `.fluent/`,
THE SYSTEM SHALL create `.fluent/` with the initial directory structure.
Test: tests/binary.rs (init_creates_fluent_structure)

### B2

WHEN `fluent init` is invoked at a Git repository root without `.fluent/`,
THE SYSTEM SHALL write a `.fluent/.gitignore`.
Test: tests/binary.rs (init_writes_gitignore_when_absent)

### B3

WHEN `fluent init` is invoked at a Git repository root,
THE SYSTEM SHALL NOT create or update an interactive-agent skill directory.
Test: tests/binary.rs (init_does_not_install_interactive_agent_skills)

### B4

WHEN `fluent init` is invoked at a Git repository root that already has `.fluent/`,
THE SYSTEM SHALL print "Already initialized."
Test: tests/binary.rs (init_is_idempotent)

### B5

WHEN `fluent init` is invoked at a Git repository root that already has `.fluent/`
and `.fluent/.gitignore` is absent,
THE SYSTEM SHALL write a `.fluent/.gitignore`.
Test: tests/binary.rs (init_backfills_gitignore_on_existing_fluent)

### B6

WHEN configured `fluent init` saves a Writer and reviewer coder profile,
THE SYSTEM SHALL NOT use either provider as an interactive-agent skill target.
Test: tests/binary.rs (configured_init_does_not_use_coder_profile_as_skill_target)

### B8

IF `fluent init` is invoked outside a Git working tree,
THEN THE SYSTEM SHALL make no project, Fluent-data, or agent-skill changes. When the immediate
`<project>/main` child is a Git repository root, THE SYSTEM SHALL print
`cd <project>/main && fluent init` as the recovery command.
Test: tests/binary.rs (init_outside_git_makes_no_changes)
Test: tests/binary.rs (init_outside_git_suggests_nested_main_repository_without_changes)

IF `fluent init` is invoked below a Git repository root,
THEN THE SYSTEM SHALL make no project, Fluent-data, or agent-skill changes and name the repository
root where initialization can run.
Test: tests/binary.rs (init_below_repository_root_makes_no_changes_and_names_root)

### B9

WHEN `fluent init` runs at a Git repository root with pre-existing Git-visible
changes, THE SYSTEM SHALL report those changes separately from managed
agent-instruction files whose bytes that invocation changed.
Test: tests/binary.rs (init_distinguishes_preexisting_dirt_from_instruction_changes)
Test: tests/binary.rs (init_does_not_claim_unchanged_dirty_instruction_file)

### B10

WHEN configured `fluent init` runs at a Git repository root, THE SYSTEM SHALL
verify that Git can track the required Expertise, hook, Dockerfile, Tester
configuration, and Tester extractor paths before host-sandbox or provider
preflight and before any project or agent-skill mutation.
Test: tests/binary.rs (configured_init_rejects_broad_fluent_ignore_before_side_effects)

### B11

IF an effective repository, nested, `.git/info/exclude`, or global ignore rule
hides a required durable Fluent path, THEN THE SYSTEM SHALL reject configured
initialization with `FLUENT_INIT_DURABLE_PATH_IGNORED`.
Test: tests/binary.rs (configured_init_reports_all_git_ignore_sources)

### B12

IF configured initialization rejects an effective ignore rule, THEN THE SYSTEM
SHALL report the affected path plus the rule's source, line, and pattern.
Test: tests/binary.rs (configured_init_reports_all_git_ignore_sources)

### B13

IF an effective negation makes every required durable Fluent path trackable, THEN
THE SYSTEM SHALL accept the ignore configuration and continue configured
initialization.
Test: tests/binary.rs (configured_init_accepts_negated_durable_paths_and_runtime_only_ignores)

### B14

IF configured initialization rejects an ignore conflict, THEN THE SYSTEM SHALL
recommend Fluent's generated `.fluent/.gitignore` or equally narrow exclusions
such as `.fluent/work/` instead of a broad `.fluent/` ignore.
Test: tests/binary.rs (configured_init_rejects_broad_fluent_ignore_before_side_effects)

---

## Init seeds operator craft

### B1

WHEN `fluent init` runs and `AGENTS.md` and/or `CLAUDE.md` exist,
THE SYSTEM SHALL append the craft section (as a managed block) to each
such file that does not already contain it.
Test: tests/binary.rs (init_appends_craft_section_to_existing_agents_md)

### B2

WHEN `fluent init` runs and neither `AGENTS.md` nor `CLAUDE.md` exists,
THE SYSTEM SHALL create `AGENTS.md` containing the craft section.
Test: tests/binary.rs (init_creates_agents_md_with_craft_section_when_none)

### B3

WHEN the managed block already exists between its markers,
THE SYSTEM SHALL update the craft section in place without duplicating
it, leaving all content outside the markers unchanged.
Test: tests/binary.rs (init_updates_craft_section_in_place_idempotently)

### B4

WHEN the craft section is written,
THE SYSTEM SHALL name the fluent skill as the authoritative source and
summarize the lifecycle (brief → behaviors → approach → plan → delegated
execute → review → Learner → ready Merge Candidate → human inspection and
land), capturing durable learning and observations, and pausing to
`needs-user` when a decision needs a human.
Test: tests/binary.rs (craft_section_names_skill_and_lifecycle_stages)

### B5

IF an agent-instructions file cannot be written,
THEN THE SYSTEM SHALL print a warning and continue so `fluent init`
still succeeds.
Test: tests/binary.rs (init_succeeds_when_craft_section_write_fails)

### B6

WHEN `fluent init` changes one or more managed `AGENTS.md` or `CLAUDE.md`
files in a Git source checkout,
THE SYSTEM SHALL name each changed instruction file and tell the user to
commit or revert it before running an Attempt because candidate worktrees
use committed Git state.
Test: tests/binary.rs (init_names_instruction_changes_that_require_git_resolution)
Test: tests/binary.rs (init_reports_changed_instructions_after_partial_seeding_failure)

### B7

WHEN `fluent init` finds that every managed instruction file already
contains the current Fluent block,
THE SYSTEM SHALL leave those files byte-for-byte unchanged and SHALL NOT
claim that they require a new Git resolution.
Test: tests/binary.rs (reinit_with_current_instructions_reports_no_new_instruction_changes)

---

## Local Preview guidance

### B1

WHEN Fluent bundles the full fluent skill,
THE SYSTEM SHALL teach that Local Preview Attempts run locally in the
foreground.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B2

WHEN Fluent bundles the full fluent skill,
THE SYSTEM SHALL teach that corrective follow-up Work is proposed by default.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B3

WHEN Fluent bundles the full fluent skill,
THE SYSTEM SHALL teach that `fluent work-item authorize` authorizes proposed
Work.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B3a

WHEN Fluent bundles the full fluent skill,
THE SYSTEM SHALL teach that `fluent work-item authorize` enqueues proposed
Work.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B4

WHEN the bundled full skill describes Work authorization,
THE SYSTEM SHALL state that authorization does not run an Attempt.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B5

WHEN the bundled full skill describes Work authorization,
THE SYSTEM SHALL state that authorization never authorizes landing.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B6

WHEN Fluent bundles the full fluent skill,
THE SYSTEM SHALL teach that queued Work starts only while a human explicitly
runs `fluent scheduler run`.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B7

WHEN Fluent bundles the full fluent skill,
THE SYSTEM SHALL teach that scheduler execution never lands a candidate and,
after successful Learning, stops at a ready Merge Candidate.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B8

WHEN Fluent bundles the full fluent skill,
THE SYSTEM SHALL teach that a human inspects and lands every ready Merge
Candidate.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B9

WHEN Fluent bundles the full fluent skill,
THE SYSTEM SHALL teach that post-merge review is off by default.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B10

WHEN Fluent bundles the full fluent skill,
THE SYSTEM SHALL teach that `--post-merge-review` is a positive per-land
opt-in.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B11

WHEN `fluent init` writes its managed agent-instruction block,
THE SYSTEM SHALL teach that Local Preview Attempts run locally in the
foreground.
Test: tests/binary.rs (init_seeds_local_preview_operating_boundary)

### B12

WHEN `fluent init` writes its managed agent-instruction block,
THE SYSTEM SHALL teach that corrective follow-up Work is proposed by default.
Test: tests/binary.rs (init_seeds_local_preview_operating_boundary)

### B13

WHEN `fluent init` writes its managed agent-instruction block,
THE SYSTEM SHALL teach that `fluent work-item authorize` authorizes and queues
Work.
Test: tests/binary.rs (init_seeds_local_preview_operating_boundary)

### B14

WHEN `fluent init` writes its managed agent-instruction block,
THE SYSTEM SHALL teach that queued Work starts only when a human runs
`fluent scheduler run`.
Test: tests/binary.rs (init_seeds_local_preview_operating_boundary)

### B15

WHEN `fluent init` writes its managed agent-instruction block,
THE SYSTEM SHALL teach that scheduler execution stops at a ready Merge
Candidate only after successful Learning.
Test: tests/binary.rs (init_seeds_local_preview_operating_boundary)

### B16

WHEN `fluent init` writes its managed agent-instruction block,
THE SYSTEM SHALL teach that a human inspects and lands every ready Merge
Candidate.
Test: tests/binary.rs (init_seeds_local_preview_operating_boundary)

### B17

WHEN `fluent init` writes its managed agent-instruction block,
THE SYSTEM SHALL teach that post-merge review is off by default.
Test: tests/binary.rs (init_seeds_local_preview_operating_boundary)

### B18

WHEN `fluent init` writes its managed agent-instruction block,
THE SYSTEM SHALL teach that `--post-merge-review` is the positive per-land
opt-in.
Test: tests/binary.rs (init_seeds_local_preview_operating_boundary)

### B19

WHEN either Local Preview teaching surface describes excluded execution modes,
THE SYSTEM SHALL identify `fluent auto-merge`, automatic scheduler lifecycle,
automatic landing, and Fargate as outside the Local Preview.
Test: src/content.rs (bundled_fluent_skill_excludes_automatic_landing)
Test: tests/binary.rs (init_seeds_local_preview_operating_boundary)

### B20

WHEN either Local Preview teaching surface describes landing,
THE SYSTEM SHALL require human acceptance rather than autonomous landing
policy.
Test: src/content.rs (bundled_fluent_skill_excludes_automatic_landing)
Test: tests/binary.rs (init_seeds_local_preview_operating_boundary)

### B21

WHEN the full fluent skill detects that `.fluent/` does not exist,
THE SYSTEM SHALL offer both the `propose` or `execute` follow-up-mode choice
and the explicit curated or custom coder-profile choice before running
configured `fluent init`.
Test: src/content.rs (bundled_fluent_skill_offers_coder_profiles_before_init)

### B22

WHEN `.fluent/` does not exist and the full fluent skill offers setup choices,
THE SYSTEM SHALL run configured `fluent init` only after the user completes the
follow-up mode and coder profile.
Test: src/content.rs (bundled_fluent_skill_orders_profile_preflight_before_init)

### B23

WHERE the user chooses `propose` during first-time project setup,
THE SYSTEM SHALL persist the selected coder profile without adding a follow-up
mapping.
Test: tests/binary.rs (coder_profile_apply_propose_writes_only_coder_mapping)

### B24

WHERE the user chooses `execute` during first-time project setup,
THE SYSTEM SHALL persist the selected profile and nested
`follow-up: { mode: execute }` mapping.
Test: tests/binary.rs (coder_profile_apply_execute_merges_profile_and_follow_up_mode)

### B25

WHERE the user chooses `execute` during first-time project setup,
THE SYSTEM SHALL explain that execute authorizes Work.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B25a

WHERE the user chooses `execute` during first-time project setup,
THE SYSTEM SHALL explain that execute queues Work.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B25b

WHERE the user chooses `execute` during first-time project setup,
THE SYSTEM SHALL explain that execute does not start Work.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B26

WHERE the user chooses `execute` during first-time project setup,
THE SYSTEM SHALL explain that a separately started scheduler executes the
queued Work.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

### B27

WHERE the user chooses `execute` during first-time project setup,
THE SYSTEM SHALL explain that the resulting candidate still requires human
inspection and landing.
Test: src/content.rs (bundled_fluent_skill_documents_local_preview_boundary)

---

## First-time coder profile setup

### B1

WHEN configured `fluent init` receives `codex-balanced` or `codex-stronger`,
THE SYSTEM SHALL resolve the documented Codex model at medium effort for both
writer and reviewer.
Test: src/setup.rs (curated_profiles_resolve_exact_writer_and_reviewer_mappings)

### B1a

WHEN configured `fluent init` receives `claude-balanced`,
THE SYSTEM SHALL resolve Claude's provider-default model at medium effort for
both writer and reviewer.
Test: src/setup.rs (curated_profiles_resolve_exact_writer_and_reviewer_mappings)

### B1b

WHEN configured `fluent init` receives a mixed-provider curated profile,
THE SYSTEM SHALL assign the named writer and reviewer providers with their
balanced model and effort settings.
Test: src/setup.rs (curated_profiles_resolve_exact_writer_and_reviewer_mappings)

### B1c

WHEN configured `fluent init` receives explicit writer or reviewer fields with
a curated profile,
THE SYSTEM SHALL replace only the supplied profile fields.
Test: src/setup.rs (curated_profile_accepts_explicit_role_overrides)

### B1d

WHEN configured `fluent init` receives `custom`,
THE SYSTEM SHALL require coder, model, and effort values for both writer and
reviewer.
Test: src/setup.rs (configured_setup_rejects_incomplete_custom_flags)

### B1e

WHEN configured `fluent init` receives a complete `custom` profile,
THE SYSTEM SHALL resolve the supplied writer and reviewer values.
Test: src/setup.rs (complete_custom_profile_resolves_both_roles)

### B1f

WHEN configured `fluent init` resolves a coder profile,
THE SYSTEM SHALL preflight every distinct selected provider before the first
initialization mutation.
Test: tests/binary.rs (mixed_coder_profile_preflights_both_providers)
Test: tests/binary.rs (claude_writer_profiles_preflight_only_selected_providers)

### B2

WHEN configured init preflights Codex,
THE SYSTEM SHALL check command and authentication readiness without requesting
model inference or consuming model quota.
Test: tests/binary.rs (codex_profile_preflight_does_not_invoke_model)

### B3

IF configured-init provider preflight fails,
THEN THE SYSTEM SHALL report the failed provider, leave a new project
uninitialized, and direct the user to retry or choose a different profile.
Test: tests/binary.rs (coder_profile_preflight_failure_preserves_uninitialized_project)
Test: src/content.rs (bundled_fluent_skill_offers_preflight_retry_or_reselection)

### B4

WHEN configured initialization succeeds,
THE SYSTEM SHALL atomically merge the complete two-role profile into project
configuration while preserving unrelated keys.
Test: tests/binary.rs (coder_profile_apply_merges_both_roles_and_preserves_unrelated_config)

### B4a

WHERE configured initialization uses `execute`,
THE SYSTEM SHALL add `follow-up.mode: execute` to project configuration.
Test: tests/binary.rs (coder_profile_apply_execute_merges_profile_and_follow_up_mode)

### B5

IF Fluent cannot save the complete coder profile,
THEN THE SYSTEM SHALL retain the prior configuration without a partial role
mapping and report that setup is incomplete.
Test: tests/binary.rs (coder_profile_apply_failure_never_leaves_partial_mapping)

### B6

WHEN first-time coder-profile setup completes,
THE SYSTEM SHALL show the saved writer and reviewer mapping.
Test: src/content.rs (bundled_fluent_skill_reports_saved_profile_and_attempt_boundary)

### B6a

WHEN first-time coder-profile setup completes,
THE SYSTEM SHALL explain that new Attempts freeze the effective mapping unless
explicit Attempt overrides are supplied.
Test: src/content.rs (bundled_fluent_skill_reports_saved_profile_and_attempt_boundary)

### B7

IF coder configuration names the retired `behavior-tests` role,
THEN THE SYSTEM SHALL reject it.
Test: src/config.rs (rejects_retired_behavior_test_coder_config)
Test: src/config.rs (configured_profile_rejects_retired_role_without_changing_config)
Test: tests/binary.rs (attempt_create_rejects_retired_behavior_test_coder_config)

### B7a

IF a persisted coder mapping names the retired `behavior-tests` role,
THEN THE SYSTEM SHALL reject it.
Test: src/work_model.rs (coder_mapping_rejects_retired_behavior_test_role)

### B7b

IF a Task kind names the retired `behavior-tests` role,
THEN THE SYSTEM SHALL reject it.
Test: src/work_model.rs (task_kind_parses_generic_kinds)

### B7c

IF a command-line argument names the retired `behavior-tests` role,
THEN THE SYSTEM SHALL reject it.
Test: tests/binary.rs (init_rejects_retired_behavior_test_role_flag)
Test: tests/binary.rs (run_commands_reject_retired_role_flags_before_project_lookup)

---

## Skill installation

### B1

WHEN `fluent skills add` runs with an explicit agent and scope,
THE SYSTEM SHALL materialize the full fluent skill — its `SKILL.md` and
`references/` — from the running binary into the target skill
directories, together with the review skills, and SHALL record one managed
installation sidecar for each selected agent, scope, and skill identity.
Test: tests/binary.rs (skills_add_materializes_full_skill_and_references,
skills_add_leaves_one_managed_installation_per_agent_and_scope)

### B2

WHEN `npx skills add mrinalwadhwa/fluent --skill fluent` runs,
THE SYSTEM SHALL install the fluent shim, not the full fluent skill,
alongside the self-contained review skills.
Test: tests/behaviors/operations/test-plugin-install.sh

### B3

WHEN `fluent skills add` runs without `--agent`, `--project`, or `--global`,
THE SYSTEM SHALL materialize the full skill bundle in the fixed handoff data
directory.
Test: tests/binary.rs (skills_add_writes_to_data_directory)

### B3a

WHEN bare `fluent skills add` finds an existing managed installation,
THE SYSTEM SHALL refresh that installation from the running bundle.
Test: tests/binary.rs (skills_add_updates_prior_managed_release_without_digest_allowlist)

### B3b

WHEN bare `fluent skills add` runs without an existing Fluent installation or
bootstrap, THE SYSTEM SHALL NOT create an interactive-agent skill root.
Test: tests/binary.rs (skills_add_bare_does_not_create_agent_roots,
skills_add_bare_refreshes_without_creating_agent_roots)

### B4

WHEN `fluent skills add` runs with `--agent <agents> -g`/`--global`,
THE SYSTEM SHALL scope the install to the named agents' global skill directories
and SHALL NOT update any project-level installation.
Test: tests/binary.rs (skills_add_global_flag_skips_project)
Test: tests/binary.rs (skills_add_explicit_scope_does_not_adopt_other_scope_shims)

### B5

WHEN `fluent skills add` runs with `--agent <agents> --project`,
THE SYSTEM SHALL scope the install to the named agents' project skill directories
and SHALL NOT install to global directories.
Test: tests/binary.rs (skills_add_project_flag_targets_project)
Test: tests/binary.rs (skills_add_explicit_scope_does_not_adopt_other_scope_shims)

### B6

IF `fluent skills add` names an agent without a scope, THEN THE SYSTEM SHALL
reject the command before creating an agent root or materializing handoff data.
Test: tests/binary.rs (skills_add_requires_a_scope_with_an_agent)

### B6a

IF `fluent skills add` names a scope without an agent, THEN THE SYSTEM SHALL
reject the command before creating an agent root or materializing handoff data.
Test: tests/binary.rs (skills_add_requires_an_agent_with_a_scope)

### B6b

IF `fluent skills add` names an unsupported agent, THEN THE SYSTEM SHALL reject
the command before creating an agent root or materializing handoff data.
Test: tests/binary.rs (skills_add_rejects_unknown_agent_without_creating_a_root)

### B7

WHEN `fluent skills show <name>` is invoked,
THE SYSTEM SHALL print the named skill's SKILL.md content to stdout.
Test: tests/binary.rs (skills_show_prints_skill_content)

### B8

WHEN `fluent skills show <name> --path` is invoked,
THE SYSTEM SHALL print the absolute path to the named skill's SKILL.md
to stdout without printing the file content.
Test: tests/binary.rs (skills_show_prints_skill_path)

### B9

WHEN a managed skill from an earlier Fluent release still matches its sidecar,
THE SYSTEM SHALL update it to the running bundle without requiring the earlier
bundle digest to be known by the running binary.
Test: tests/binary.rs (skills_add_updates_prior_managed_release_without_digest_allowlist)

### B10

IF a sidecar-bearing installation does not have a valid sidecar for the
selected agent, scope, skill, and complete file inventory, or an installation
has no recognized ownership record,
THEN THE SYSTEM SHALL preserve the directory byte-for-byte and report its path
with cleanup guidance, even when it otherwise matches a legacy adoption route.
Test: tests/binary.rs (skills_add_preserves_edited_added_and_missing_managed_files,
skills_add_preserves_malformed_and_identity_mismatched_sidecars,
skills_add_preserves_invalid_sidecar_on_shim_marked_installation,
skills_add_preserves_invalid_sidecar_on_scanned_shim_installation,
skills_add_reports_unmarked_conflict_without_modifying_it)

### B10a

IF a bare `fluent skills add` finds a user-owned Fluent directory, THEN THE
SYSTEM SHALL NOT install sibling bundled skills into that agent root.
Test: tests/binary.rs (skills_add_does_not_clobber_unmarked_directory)

### B11

WHEN both global and project managed Fluent installations exist,
THE SYSTEM SHALL preserve both and report their scopes and paths, including
that the agent may display both entries.
Test: tests/binary.rs (skills_add_preserves_and_reports_cross_scope_installations)

### B12

WHEN a managed installation already matches the running bundle,
THE SYSTEM SHALL leave its files unchanged and report that it is current
without an installation-failure warning.
Test: tests/binary.rs (skills_add_does_not_rewrite_current_managed_installation,
skills_add_current_installation_reports_success_without_warning)

### B13

WHEN Fluent finds a sidecar-free exact allowlisted pre-sidecar bundle or a
sidecar-free Fluent-marked shim,
THE SYSTEM SHALL replace it with a managed installation of the running bundle.
Test: src/skill_install.rs (adopts_an_exact_allowlisted_bundle_and_rejects_a_near_match)
Test: src/skill_install.rs (fluent_shim_marker_must_be_in_frontmatter)
Test: tests/binary.rs (skills_add_replaces_stale_shim_installation)
Test: tests/binary.rs (skills_add_converges_project_bootstrap_roots_and_lock)

### B14

WHEN `fluent skills add` replaces a sidecar-free Fluent-marked shim in a
selected target,
THE SYSTEM SHALL report that it replaced a Fluent shim in that target's outcome
summary.
Test: tests/binary.rs (skills_add_replaces_stale_shim_installation)

### B15

WHEN `fluent skills add` installs bundled skills into a selected target that
did not contain a Fluent shim,
THE SYSTEM SHALL NOT report a Fluent shim replacement for that target.
Test: tests/binary.rs (fluent_skills_install_writes_all_public_skills)

### B16

WHEN `fluent skills add` runs with an explicit agent and scope,
THE SYSTEM SHALL NOT adopt or modify Fluent installations outside the selected
targets.
Test: tests/binary.rs (skills_add_explicit_scope_does_not_adopt_other_scope_shims)

### B17

WHEN Fluent adopts a shim whose matching external lock contains a recognized
Fluent bootstrap entry,
THE SYSTEM SHALL remove the Fluent entry.
Test: src/skill_install.rs (external_lock_handoff_removes_only_the_fluent_entry)
Test: tests/binary.rs (skills_add_converges_project_bootstrap_roots_and_lock)
Test: tests/binary.rs (skills_add_converges_global_canonical_roots_and_lock)

### B17a

WHEN Fluent removes its entry from an external skill lock,
THE SYSTEM SHALL preserve every unrelated lock value.
Test: src/skill_install.rs (external_lock_handoff_removes_only_the_fluent_entry)
Test: tests/binary.rs (skills_add_converges_project_bootstrap_roots_and_lock)
Test: tests/binary.rs (skills_add_converges_global_canonical_roots_and_lock)

### B18

IF a detected shim's external lock is malformed or attributes `fluent` to an
unrecognized source or skill path,
THEN THE SYSTEM SHALL reject the installation before mutating any data or agent
skill directory.
Test: src/skill_install.rs (external_lock_handoff_preserves_unrecognized_or_invalid_locks)
Test: tests/binary.rs (skills_add_preserves_shim_when_external_lock_is_invalid)

### B18a

IF Fluent rejects an installation because its external lock is malformed or
attributes `fluent` to an unrecognized source or skill path,
THEN THE SYSTEM SHALL leave the detected shim and external lock unchanged.
Test: tests/binary.rs (skills_add_preserves_shim_when_external_lock_is_invalid)

### B19

IF Fluent removes an external Fluent lock entry but cannot replace the shim,
THEN THE SYSTEM SHALL restore the original external lock.
Test: src/main.rs (failed_shim_replacement_restores_external_lock)

### B20

WHEN `fluent skills add` runs again after adopting one or more project bootstrap
roots,
THE SYSTEM SHALL leave the released external lock unchanged.
Test: tests/binary.rs (skills_add_converges_project_bootstrap_roots_and_lock)

### B20a

WHEN `fluent skills add` discovers an unmarked or invalid Fluent directory
outside its selected install targets,
THE SYSTEM SHALL NOT report that directory as a managed Fluent installation.
Test: tests/binary.rs (skills_add_does_not_report_user_owned_discovery_as_managed)

## Documentation review — commit conventions

### B1

WHEN the documentation reviewer reviews a normal Attempt candidate and
the project states commit-message conventions in `AGENTS.md` or
`CLAUDE.md`,
THE SYSTEM SHALL treat subject length and style, body presence, wrapping,
formatting, and literal `\n` text as non-blocking presentation advice and SHALL
NOT emit a finding or failing verdict solely for that advice, while continuing
to flag violations that affect linear history, identity, message encoding, or attribution,
including prohibited `Co-Authored-By` trailers.
Test: src/work_task_executor.rs (documentation_review_treats_commit_presentation_as_advice)

---

## Post-merge forward-fix landing

### B1

WHEN the post-merge review builds a post-merge-review-fix Work Item's
Attempt to a passing Merge Candidate,
THE SYSTEM SHALL leave that Merge Candidate pending and SHALL NOT land
it; the candidate is landed only by a later explicit land — the
`auto-merge` watcher or `fluent merge-candidate land` — exactly like
any other candidate.
Untestable: Requires an end-to-end post-merge review Attempt reaching a Merge Candidate with LLM reviewers.

### B2

WHEN the post-merge review running at fix depth N auto-creates a
post-merge-review-fix Work Item,
THE SYSTEM SHALL record fix depth N+1 on that Work Item's persisted
state, so a later land of its Merge Candidate — performed by any
process, including the separate `auto-merge` watcher — resumes at the
correct fix depth.
Test: src/post_merge_review.rs (fix_work_item_records_incremented_fix_depth)

### B3

WHEN a Merge Candidate is landed and the merge executor spawns a
post-merge review for that land,
THE SYSTEM SHALL derive the fix depth from the landed Merge
Candidate's Work Item persisted state — depth 0 when the Work Item
records none — independent of any process environment variable.
Test: src/post_merge_review.rs (fix_depth_defaults_to_zero_for_non_fix_work_item)

### B4

WHERE a landed Merge Candidate's Work Item is at fix depth at or above
the cap `FLUENT_MAX_POST_MERGE_REVIEW_FIX_DEPTH` (default 5),
THE SYSTEM SHALL NOT spawn a post-merge review for that land, bounding
the forward-fix recursion the same way across in-process and
cross-process lands.
Test: src/post_merge_review.rs (does_not_spawn_post_merge_review_at_or_above_fix_depth_cap)

### B5

WHEN a Work Item carrying a recorded fix depth is written and re-read
through the Work model store,
THE SYSTEM SHALL preserve the fix depth.
Test: src/work_model.rs (work_item_fix_depth_round_trips_through_storage)

---

## Post-merge review daemon

A per-target-branch singleton lease bounds the post-merge review daemon
to one live process. Only a fresh land with the affirmative
`--post-merge-review` option enqueues a review job; omission is the default-off
path. The drain-all-on-wake behavior is unchanged. These statements add the
singleton guard that stops opted-in lands from leaking a new sleeping daemon.

### B1

WHEN a land completes and enqueues a post-merge review job, and a live
post-merge review daemon already holds the singleton lease for the
target branch,
THE SYSTEM SHALL NOT spawn another daemon; the running daemon drains the
job on its next wake.
Test: src/post_merge_review.rs (skips_spawn_when_daemon_lease_held_live)

### B2

WHEN a post-merge review daemon starts and a live daemon already holds
the lease,
THE SYSTEM SHALL exit immediately without draining.
Test: src/post_merge_review.rs (second_daemon_exits_when_lease_held)

### B3

WHERE the singleton lease is held by a process that is no longer
running,
THE SYSTEM SHALL treat it as free so a land or daemon can acquire it and
proceed.
Test: src/post_merge_review.rs (stale_lease_is_treated_as_free)

### B4

WHEN the post-merge review daemon exits,
THE SYSTEM SHALL release the singleton lease.
Test: src/post_merge_review.rs (daemon_releases_lease_on_exit)

---

## Install script

### B1

WHEN the install script runs on a supported platform,
THE SYSTEM SHALL install the current release binary to
`~/.local/bin/fluent` so that `fluent version` runs successfully.
Test: tests/behaviors/operations/test-install-script.sh (installs a runnable binary)

### B2

IF the operating-system and architecture pair is unsupported, THEN THE SYSTEM
SHALL report the platform is unsupported and exit without installing.
Test: tests/behaviors/operations/test-install-script.sh (unsupported platform)

### B3

WHEN the install script installs and `--no-modify-path` is not set,
THE SYSTEM SHALL add `~/.local/bin` to PATH through the user's shell
configuration and report that the shell must be reloaded.
Test: tests/behaviors/operations/test-install-script.sh (modifies PATH)

### B4

WHERE `--no-modify-path` is set and `~/.local/bin` is not already on
PATH,
THE SYSTEM SHALL warn the user to add it manually and make no
shell-configuration change.
Test: tests/behaviors/operations/test-install-script.sh (no-modify-path warns)

### B5

IF the download fails,
THEN THE SYSTEM SHALL report the failure and exit without leaving a
partial binary.
Test: tests/behaviors/operations/test-install-script.sh (download failure)

### B6

IF the downloaded release binary does not match its published SHA-256 checksum,
THEN THE SYSTEM SHALL fail installation and preserve any installed binary.
Test: tests/behaviors/operations/test-install-script.sh (checksum mismatch preserves installed binary)

### B7

WHEN the install script runs on Apple Silicon macOS, THE SYSTEM SHALL download
the `fluent-aarch64-apple-darwin` binary and its matching checksum.
Test: tests/behaviors/operations/test-install-script.sh (Apple Silicon selects aarch64 asset)

### B8

WHEN the install script runs on Intel macOS, THE SYSTEM SHALL download the
`fluent-x86_64-apple-darwin` binary and its matching checksum.
Test: tests/behaviors/operations/test-install-script.sh (Intel selects x86_64 asset)

### B9

IF a release omits the binary for the current supported Mac architecture, THEN
THE SYSTEM SHALL fail installation and preserve any installed binary.
Test: tests/behaviors/operations/test-install-script.sh (missing Intel asset preserves installed binary)

---

## Clean-room first-use harness

### B1

IF `first-run-smoke.sh prepare` receives no curated coder profile or an
unsupported profile,
THEN THE SYSTEM SHALL reject the request before creating the smoke root.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (prepare requires a supported profile)

### B2

WHEN `first-run-smoke.sh prepare` accepts a curated coder profile,
THE SYSTEM SHALL record that exact profile in the manifest.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (prepare records every curated profile)

### B2a

WHEN `first-run-smoke.sh prepare` completes,
THE SYSTEM SHALL record a schema-v2 manifest.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (prepare is isolated)

### B2b

IF `first-run-smoke.sh prepare` targets a completed smoke root with another
recorded coder profile,
THEN THE SYSTEM SHALL reject the request without changing the manifest.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (prepare records every curated profile)

### B2c

IF `first-run-smoke.sh run` reads an unsupported coder profile from a schema-v2
manifest,
THEN THE SYSTEM SHALL reject it before invoking Fluent.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (prepare records every curated profile)

### B3

WHEN `first-run-smoke.sh prepare` creates the fixture repository,
THE SYSTEM SHALL seed a marked project Fluent bootstrap.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (prepare is isolated)

### B3a

WHEN `first-run-smoke.sh prepare` seeds the project Fluent bootstrap,
THE SYSTEM SHALL seed a matching Fluent entry in the external lock.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (prepare is isolated)

### B3b

WHEN `first-run-smoke.sh prepare` seeds the external skill lock,
THE SYSTEM SHALL include an unrelated fixture skill entry.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (prepare is isolated)

### B4

WHEN `first-run-smoke.sh run` reaches the skill-bootstrap checkpoint,
THE SYSTEM SHALL require the candidate binary to replace the marked bootstrap
before initialization.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run configures init and commits before attempt)
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (skill handoff rejects wrong ownership and resumes)
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (skill handoff failure preserves evidence and resumes)

### B4a

WHEN the candidate binary replaces the clean-room bootstrap,
THE SYSTEM SHALL require a matching Fluent ownership sidecar before initialization.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run configures init and commits before attempt)
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (skill handoff rejects wrong ownership and resumes)

### B4b

WHEN the candidate binary replaces the clean-room bootstrap,
THE SYSTEM SHALL require Fluent's external lock entry to be absent before
initialization.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run configures init and commits before attempt)

### B4c

WHEN the candidate binary releases Fluent's external lock entry,
THE SYSTEM SHALL require every unrelated fixture lock value to remain before
initialization.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run configures init and commits before attempt)

### B4c1

WHEN the candidate binary replaces the clean-room bootstrap with a bare skill
handoff command, THE SYSTEM SHALL NOT create a project Codex or Claude Code
skill root that the bootstrap did not select.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run configures init and commits before attempt)

### B4d

WHEN `first-run-smoke.sh run` commits initialized fixture state, THE SYSTEM SHALL
include an executable Tester extractor with the Tester configuration before it
creates the Work Item.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run configures init and commits before attempt)

### B4e

WHEN the fixture Tester extractor receives the result of `./check.sh`, THE
SYSTEM SHALL report one `first-run-smoke/greeting` test whose status is `fail`
for a nonzero exit and `pass` for a zero exit.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (tester extractor normalizes fixture contract)

### B4f

WHEN Fluent validates the fixture Tester contract with no command results, THE
SYSTEM SHALL return an empty test array.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (tester extractor normalizes fixture contract)

### B4g

IF the fixture Tester extractor receives malformed evidence or a command other
than the declared fixture check, THEN THE SYSTEM SHALL exit nonzero without
emitting normalized test evidence.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (tester extractor normalizes fixture contract)

### B5

WHEN the fixture Work Item and planned Attempt exist,
THE SYSTEM SHALL add the Work Item to the queue.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run uses public sequence and stops before land)

### B5a

WHEN the harness observes the planned Attempt waiting for scheduler dispatch,
THE SYSTEM SHALL start `fluent scheduler run`.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run uses public sequence and stops before land)

### B5b

WHEN the harness executes the planned Attempt through the scheduler queue,
THE SYSTEM SHALL NOT invoke `fluent attempt run` directly.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run uses public sequence and stops before land)

### B5c

WHEN a scheduler-owned dispatch reaches terminal status while the harness is
waiting, THE SYSTEM SHALL observe that status after the dispatch leaves the
active queue.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run uses public sequence and stops before land)
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (scheduler exit rereads terminal queue state)

### B5d

IF `fluent queue show` returns invalid JSON, THEN THE SYSTEM SHALL stop the run
through its resumable failure path.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (malformed queue show preserves evidence and resumes)

### B6

WHEN the scheduler-owned dispatch reaches a terminal queue status,
THE SYSTEM SHALL leave no harness-started scheduler process running.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run uses public sequence and stops before land)
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (scheduler exit rereads terminal queue state)

### B6a

WHEN the harness is interrupted while its scheduler child is running,
THE SYSTEM SHALL leave no harness-started scheduler process running.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (interruption reaps scheduler child)

### B6b

WHEN the scheduler-owned dispatch reaches `candidate-ready`,
THE SYSTEM SHALL NOT land its Merge Candidate during `run`.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run uses public sequence and stops before land)

### B7

WHEN a scheduler-owned dispatch stops at `needs-user` and the operator reruns
the harness after resolving the cause,
THE SYSTEM SHALL queue one resume for the manifest's exact Attempt.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run resumes same attempt after scheduler pause)
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (failure preserves evidence and resume)

### B7a

WHEN the harness resumes a scheduler-owned dispatch,
THE SYSTEM SHALL NOT repeat configured initialization.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (failure preserves evidence and resume)

### B7b

WHEN the harness resumes a scheduler-owned dispatch,
THE SYSTEM SHALL NOT recreate the Work Item.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (failure preserves evidence and resume)

### B7c

WHEN the harness resumes a scheduler-owned dispatch,
THE SYSTEM SHALL NOT recreate the Attempt.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (failure preserves evidence and resume)

### B7d

WHEN the harness resumes a scheduler-owned dispatch,
THE SYSTEM SHALL NOT repeat the original queue dispatch.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (failure preserves evidence and resume)

### B8

WHEN the queue reports `candidate-ready`,
THE SYSTEM SHALL require the stored Attempt to be complete before presenting
candidate inspection and explicit landing commands.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run waits for complete attempt)
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (ready handoff is actionable)

### B8a

WHEN the queue reports `candidate-ready`,
THE SYSTEM SHALL require the stored Learner to have succeeded before presenting
candidate inspection and explicit landing commands.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (run waits for learner success)
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (ready handoff is actionable)

### B9

IF a prepared smoke root contains a schema-v1 manifest,
THEN THE SYSTEM SHALL reject it.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (schema one manifest is rejected without migration)

### B9a

IF a prepared smoke root contains a schema-v1 manifest,
THEN THE SYSTEM SHALL NOT invoke Fluent.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (schema one manifest is rejected without migration)

### B9b

IF a prepared smoke root contains a schema-v1 manifest,
THEN THE SYSTEM SHALL NOT migrate the manifest.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (schema one manifest is rejected without migration)

### B10

WHEN the automated clean-room harness suite runs,
THE SYSTEM SHALL NOT invoke a network client.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (automated test uses only local doubles)

### B10a

WHEN the automated clean-room harness suite runs,
THE SYSTEM SHALL NOT invoke a coding agent.
Test: tests/behaviors/operations/test-first-run-smoke-harness.sh (automated test uses only local doubles)

---

## First Writer project expertise boundary

### B1

WHEN Fluent starts an initial Write Task from committed state that has no
project expertise index,
THE SYSTEM SHALL launch the Writer without a preceding project-expertise coder
invocation.
Test: tests/binary.rs (initial_claude_writer_runs_once_without_pre_writer_expertise_seed)

### B2

WHEN an initial Write Task completes from committed state that has no project
expertise index,
THE SYSTEM SHALL keep the pre-review candidate free of automatically generated
project expertise changes.
Test: tests/binary.rs (initial_claude_writer_runs_once_without_pre_writer_expertise_seed)

### B3

WHERE a project expertise index already exists,
THE SYSTEM SHALL make that committed index available to the Writer as project
expertise.
Test: src/work_task_executor.rs (write_task_prompt_includes_project_expertise_index_when_present)

## Learning capture and portable handoff

The **Learner** performs learning capture after a code-producing Attempt
passes its reviews: it refines durable project expertise and describes
possible follow-up observations. A **code-producing Attempt** has an
accepted Write result; a review-only Attempt is not code-producing. The
**learner handoff** is portable operational output containing zero or more
independently identifiable follow-ups; it is not itself an Observation or
Work Item.

The expertise-capture, canonical `Update expertise` commit, candidate-pointer,
and expertise-write-grant behaviors in this section describe the default
`capture` Learner mode. The **Selectable pre-land Learner policy** section below
defines the `no-expertise` mode, whose isolated audit denies expertise and
candidate Git writes and keeps the reviewed Writer commit exact. A merged
candidate always runs the post-land handoff-only path regardless of the stored
mode.

A **relaunchable** Learning failure permits a later
`fluent attempt run` to invoke the Learner again. Fluent derives
relaunchability from the stored `LearningFailureKind`. An **ordinary
completed-call failure** is a rejection Fluent can settle while the host
process and Work-model store remain available; host death and a Work-model
persistence outage are outside this contract.

### B1

WHEN a code-producing Attempt's reviews pass,
THE SYSTEM SHALL run the Learner before reporting the resulting Merge
Candidate as ready, regardless of whether any reviewer raised a finding.
Test: src/work_attempt_loop.rs (learner_runs_after_first_pass_without_findings)
Test: src/work_attempt_loop.rs (learner_runs_after_passing_corrective_round)

### B2

WHERE an Attempt is review-only,
THE SYSTEM SHALL NOT run the Learner for that Attempt.
Test: src/work_attempt_loop.rs (review_only_attempt_skips_learner)

### B3

WHEN the Learner runs,
THE SYSTEM SHALL provide it the complete change produced by the Attempt
and the Tester and reviewer artifacts from every review round.
Test: src/work_task_executor.rs (learner_prompt_includes_attempt_diff_and_all_review_artifacts)

### B4

WHERE a Work Item's Learner mode is `capture`,
WHEN the Learner runs before land and identifies durable project
knowledge,
THE SYSTEM SHALL merge that knowledge into `.fluent/expertise/`, refining
or adding entries without duplicating existing expertise or altering
project source or documentation.
Untestable: Whether generated knowledge is durable is an LLM judgment. The
deterministic target, prompt inputs, and write confinement are covered by
B3, B12, and B13.

### B5

WHEN the Learner evaluates project knowledge,
THE SYSTEM SHALL record durable project-level conventions, architectural
constraints, testing patterns, and gotchas rather than one-off details of
the originating change.
Untestable: Durability and project relevance are semantic judgments
constrained by the Learner prompt.

### B6

WHERE the Learner finds no durable project knowledge,
THE SYSTEM SHALL leave `.fluent/expertise/` unchanged without creating an
expertise commit, and verify the candidate workspace is exactly clean at
the baseline before accepting the no-change result.
Test: src/work_attempt_loop.rs (learner_without_expertise_change_keeps_candidate_commit)

### B7

WHEN the Learner succeeds,
THE SYSTEM SHALL persist exactly one learner handoff associated with the
originating Work Item, Attempt, and resulting Merge Candidate, containing
zero or more stable, independently identifiable follow-ups.
Test: tests/binary.rs (learner_persists_empty_handoff)
Test: tests/binary.rs (learner_persists_followup_handoff_with_origin_provenance)

### B8

WHEN the Learner produces a handoff,
THE SYSTEM SHALL NOT directly create an Observation, create or modify a
derived Work Item, or add an entry to the regular Work Queue.
Test: tests/binary.rs (learner_handoff_does_not_materialize_before_land)

### B10

IF the Learner fails,
THEN THE SYSTEM SHALL record a durable learner-failure state associated
with the originating Attempt, carrying its typed `LearningFailureKind`
and corresponding relaunchability, warn the operator, and still create
the Merge Candidate record while withholding its readiness and land
eligibility until the Learner succeeds.
Test: src/work_attempt_loop.rs (learner_completed_failure_matrix_persists_typed_terminal_records)
Test: tests/binary.rs (learner_failure_blocks_candidate_readiness)

### B11

WHEN a failed Learner run is retried successfully,
THE SYSTEM SHALL complete that existing learning record with one
successful handoff without duplicating any previously accepted follow-up
identity.
Test: tests/binary.rs (learner_retry_completes_existing_record_idempotently)

### B11a

WHEN `fluent attempt run <work-item-id> <attempt-id>` is invoked for an
otherwise complete code-producing Attempt whose learning record is failed with
a relaunchable disposition and whose Task history contains Complete Tasks and
valid Superseded terminal history,
THE SYSTEM SHALL retry only the Learner and its candidate or handoff
bookkeeping without rerunning the Writer, Tester, or reviewers.
Test: tests/binary.rs (learner_terminal_history_foreground_retries_only_learner)
Test: tests/binary.rs (attempt_run_retries_only_failed_learner)

### B11b

WHEN a Learner failure has the non-relaunchable `EvidencePending` disposition
because its coder ran but host evidence persistence failed,
THE SYSTEM SHALL preserve that failure and its run count on a later Attempt run,
SHALL NOT relaunch the Learner coder, and SHALL continue withholding Merge
Candidate readiness for human recovery.
Test: src/work_attempt_loop.rs (learner_sidecar_failure_recovery_calls_coder_once)

### B12

WHERE a Work Item's Learner mode is `capture`,
WHEN a pre-land Learner run leaves accepted expertise changes — committed,
staged, unstaged, or untracked — all confined to `.fluent/expertise/`,
THE SYSTEM SHALL host-author exactly one `Update expertise` commit whose
sole parent is the Learner baseline, folding the complete accounted
expertise delta (including deletions) rather than merely the Learner's
current `HEAD`, and record that commit as the Merge Candidate's candidate
commit only after verifying the workspace exactly clean.
Test: src/work_attempt_loop.rs (learner_expertise_commit_becomes_candidate_commit)
Test: src/work_attempt_loop.rs (learner_host_finalizes_uncommitted_expertise)
Test: src/work_attempt_loop.rs (learner_squashes_committed_and_residual_expertise)
Test: src/work_attempt_loop.rs (learner_hook_rewritten_topology_is_rejected_and_restored)
Test: src/work_attempt_loop.rs (learner_hook_rewritten_subject_is_rejected_and_restored)
Test: src/work_attempt_loop.rs (learner_canonicalizes_deletion_without_handoff_blob_reference)

### B13

IF a Learner leaves any accounted committed, staged, unstaged, or
untracked change outside `.fluent/expertise/` — even one a later commit
or schema repair reverted or reset out of reachable history,
THEN THE SYSTEM SHALL reject the complete Learner Git result and classify
the primary failure as relaunchable `Generic`.
Test: src/work_attempt_loop.rs (learner_commit_touching_source_is_discarded)
Test: src/work_attempt_loop.rs (learner_out_of_bounds_dirty_state_is_restored_and_blocks_readiness)
Test: src/work_attempt_loop.rs (learner_reverted_out_of_bounds_commit_is_still_rejected)

### B14

WHERE a `capture` Learner runs sandboxed before land,
THE SYSTEM SHALL grant it write access only to `.fluent/expertise/`, the
designated managed handoff surface, and Git metadata needed for an
expertise commit; it SHALL NOT grant write access to the Observation
backlog, Work model, or remaining workspace.
Test: src/work_task_executor.rs (learner_sandbox_confines_expertise_handoff_and_git_writes)

### B15

WHEN Fluent launches an effectively sandboxed Learner,
THE SYSTEM SHALL resolve and inject the selected coder's supported
credentials before entering the sandbox, without granting sandbox access
to the host credential store.
Test: tests/binary.rs (trusted_learner_hydrates_credentials_before_sandbox)

### B16

WHEN an effectively sandboxed Learner launches a coder tool that requires
temporary files,
THE SYSTEM SHALL let that tool use temporary state scoped to the current
workspace while keeping sibling workspace state, shared temporary trees, and
live project roots non-writable.
Test: tests/binary.rs (trusted_learner_has_private_temp_without_shared_temp_access)
Test: src/work_task_executor.rs (create_private_launch_temp_gives_distinct_mode_0700_dirs)
Test: src/os.rs (confined_profile_writes_only_its_claude_bash_temp_namespace)

### B17

WHEN a Claude Learner session returns an authentication rejection,
THE SYSTEM SHALL preserve a transcript, apply the existing one-refresh
retry policy, and either continue automatically or record the specific
authentication recovery state.
Test: tests/binary.rs (trusted_learner_auth_401_refreshes_from_transcript)
Test: src/coder.rs (preserve_transcript_phase_refuses_to_overwrite_an_existing_sibling)

### B18

WHEN Fluent instructs a Learner to write a handoff draft,
THE SYSTEM SHALL describe every serialized field with an example that
parses as the production `LearnerDraftV1` schema and distinguish prose
corrective evidence from digest-bearing artifact evidence.
Test: src/follow_up.rs (learner_prompt_examples_parse_as_complete_drafts)

### B19

WHILE the handoff transport cannot publish independently referenced
evidence artifacts,
THE SYSTEM SHALL instruct the Learner to emit an empty artifact-evidence
array.
Test: src/work_task_executor.rs (learner_prompt_requires_empty_artifact_evidence_until_transport_exists)

### B20

IF a non-corrective follow-up is otherwise valid but carries malformed
optional artifact evidence,
THEN THE SYSTEM SHALL preserve it as Observation-only without that evidence
and record the normalization rather than reject the whole draft.
Test: src/learner.rs (malformed_noncorrective_artifact_evidence_is_preserved_observation_only)

### B21

IF a corrective follow-up contains malformed, incomplete, or unsupported
corrective metadata,
THEN THE SYSTEM SHALL preserve the finding as Observation-only and SHALL
NOT create executable Work from it.
Test: src/learner.rs (malformed_corrective_followup_downgrades_without_batch_failure)

### B22

WHEN a Learner draft fails schema validation and the configured
schema-repair budget remains,
THE SYSTEM SHALL retry with the rejected draft and exact validation error
as a schema repair, without rerunning the semantic audit.
Test: tests/binary.rs (learner_schema_failure_repairs_prior_draft)

### B23

IF a schema repair removes a prior follow-up id or changes a prior
follow-up's non-schema content,
THEN THE SYSTEM SHALL reject that repair and retain the earlier draft.
Test: src/learner.rs (schema_repair_cannot_silently_drop_or_rewrite_followups)

### B24

WHEN a Learner draft or schema repair is rejected,
THE SYSTEM SHALL preserve the submitted bytes, full validation error, and
run identity as immutable Attempt artifacts before another draft may be
published.
Test: tests/binary.rs (learner_rejected_drafts_are_immutable_run_artifacts)

### B25

WHEN a passing code-producing Attempt is about to launch its pre-land
Learner,
THE SYSTEM SHALL verify that the managed candidate workspace has `HEAD`
at the Learner baseline with no staged, unstaged, or untracked change.
Test: src/work_attempt_loop.rs (learner_dirty_baseline_fails_closed_before_coder_launch)

### B25a

IF the transaction-entry verification fails,
THEN THE SYSTEM SHALL launch no Learner coder, leave the Write output and
Merge Candidate pointers unchanged, and classify the failure as
relaunchable `Generic`.
Test: src/work_attempt_loop.rs (learner_dirty_baseline_fails_closed_before_coder_launch)

### B26

WHEN an initial or schema-repair coder invocation returns for a pre-land
Learner,
THE SYSTEM SHALL capture and retain its returned `HEAD`, reachable
commit-history paths, staged paths, unstaged paths, and untracked paths
before inspecting the result, publishing or validating the draft, or
launching another invocation.
Test: src/work_attempt_loop.rs (learner_inventory_covers_commits_index_worktree_and_untracked)
Test: src/work_attempt_loop.rs (learner_cumulative_history_survives_schema_repair_hard_reset)

### B26a

WHEN the initial Learner invocation, every schema-repair invocation, and
final draft validation succeed,
THE SYSTEM SHALL classify the cumulative union of every retained
invocation-return snapshot with the final pre-normalization state before
accepting any Git result.
Test: src/work_attempt_loop.rs (learner_inventory_covers_commits_index_worktree_and_untracked)
Test: src/work_attempt_loop.rs (learner_cumulative_history_survives_schema_repair_hard_reset)

### B27

IF a pre-land Learner snapshot's `HEAD` is not a descendant of the
Learner baseline or its history is not an unambiguous linear
Learner-created sequence,
THEN THE SYSTEM SHALL treat the complete Learner Git result as
unaccounted and reject it.
Test: src/work_attempt_loop.rs (learner_ambiguous_history_is_rejected_and_restored)

### B27a

WHEN an accounted Learner path contains a newline,
THE SYSTEM SHALL classify the exact complete path without splitting it
or omitting any bytes.
Test: src/work_attempt_loop.rs (learner_newline_path_is_accounted_without_splitting)

### B27b

IF an accounted Learner path contains non-UTF-8 bytes,
THEN THE SYSTEM SHALL reject the complete Learner Git result rather than
classify the path lossily.
Test: src/work_attempt_loop.rs (learner_non_utf8_path_is_rejected_and_restored)

### B28

WHEN an ordinary completed-call pre-land Learner rejection occurs after
transaction-entry verification succeeds and before a clean canonical
expertise result is accepted,
THE SYSTEM SHALL restore the exact clean Learner baseline, retain the
pre-Learner pointers, persist terminal `Failed` Learning with no handoff
reference and with the original primary diagnostic, its typed
`LearningFailureKind`, and corresponding relaunchability, and withhold
Merge Candidate readiness.
Test: src/work_attempt_loop.rs (learner_completed_failure_matrix_persists_typed_terminal_records)
Test: src/work_attempt_loop.rs (learner_coder_failure_rolls_back_all_git_state_and_persists_typed_failure)
Test: src/work_attempt_loop.rs (learner_schema_repair_failure_rolls_back_all_git_state_and_persists_typed_failure)
Test: src/work_attempt_loop.rs (learner_invalid_final_draft_rolls_back_when_repair_budget_is_zero)

### B29

IF restoration cannot prove both the exact baseline `HEAD` and an empty
staged, unstaged, and untracked state,
THEN THE SYSTEM SHALL record the restoration failure as a distinctly
labelled secondary diagnostic without replacing the original primary
diagnostic, its `LearningFailureKind`, or its relaunchability.
Test: src/work_attempt_loop.rs (learner_rollback_failure_preserves_primary_kind_and_distinct_restoration_diagnostic)

### B30

WHEN Fluent records a pre-land Learner as succeeded,
THE SYSTEM SHALL expose a Write output and Merge Candidate that both name
the same clean canonical expertise result with no uncommitted portion. If the
Write output carries verified no-change evidence, THE SYSTEM SHALL preserve its
verified commit separately and record the host-owned canonicalization transition.
Test: src/work_attempt_loop.rs (learner_success_requires_complete_clean_expertise_result)
Test: src/work_attempt_loop.rs (learner_success_updates_pointers_only_after_clean_verification)
Test: src/work_attempt_loop.rs (learner_expertise_commit_preserves_no_change_writer_identity)

### B30a

IF canonical-handoff publication fails after a clean canonical expertise
result is verified,
THEN THE SYSTEM SHALL retain that result as both the Write output and
Merge Candidate candidate commit and record relaunchable `Generic`
failed Learning with no handoff reference and a
canonical-handoff-publication diagnostic plus the typed `HandoffPublication`
failure stage. A no-change Write output SHALL retain its verified commit and
canonicalization transition only in this accounted recovery state; ordinary
failed Learning SHALL NOT authorize commit divergence.
Test: src/work_attempt_loop.rs (learner_handoff_failure_retains_clean_typed_failed_result)
Test: src/work_attempt_loop.rs (learner_handoff_failure_preserves_no_change_writer_identity)

## Selectable pre-land Learner policy

A Work Item's **Learner mode** is a durable policy chosen before execution that
controls whether its pre-land Learner may capture expertise. **capture** is the
default and preserves the behavior above. **no-expertise** still audits the
accepted change and produces a handoff, but denies expertise and candidate Git
mutations and keeps the **reviewed Writer SHA** — the commit named by the latest
completed Write output, the pending Merge Candidate, and every Tester and
built-in reviewer context in the final passing review round — exact through
Learner. Earlier corrective-round contexts are historical evidence, not competing
current pointers.

Every Learner provider launch also receives `FLUENT_TASK_KIND=learner`, the
resolved `FLUENT_LEARNER_MODE`, and the exact managed
`FLUENT_LEARNER_DRAFT_PATH`. Provider integrations can identify the task and
output boundary without parsing prompt prose.

### B1

WHEN `fluent work-item create` omits `--learner-mode`, receives
`--learner-mode capture`, or reads a legacy record with no `learner_mode` field,
THE SYSTEM SHALL resolve the Work Item's Learner mode as `capture` and omit the
default field from the minimal split record.
Test: tests/binary.rs (work_item_create_defaults_learner_mode_to_capture)
Test: tests/binary.rs (work_item_create_accepts_explicit_capture_learner_mode)
Test: src/work_model.rs (learner_mode_defaults_to_capture_for_legacy_work_items)
Test: tests/work_model_external.rs (capture_mode_preserves_minimal_work_item_json)

### B2

WHEN `fluent work-item create` receives `--learner-mode no-expertise`,
THE SYSTEM SHALL persist the exact split-record field
`"learner_mode": "no-expertise"` and materialize the resolved mode through
`fluent work-item show`; an unsupported value is rejected before the Work Item is
created.
Test: src/work_model.rs (no_expertise_learner_mode_round_trips_through_split_storage)
Test: tests/binary.rs (work_item_show_materializes_resolved_learner_mode)
Test: tests/binary.rs (work_item_create_rejects_unknown_learner_mode)

### B3

WHILE a code-producing Work Item's Learner mode is `no-expertise`,
WHEN its final review round passes,
THE SYSTEM SHALL run Learner and require successful Learning before reporting the
Merge Candidate ready, persist a valid handoff whose expertise reference list is
empty, describe missing durable knowledge only as non-corrective follow-up
material that may later become an Observation, and create no expertise commit or
tracked candidate-worktree change.
Test: src/work_attempt_loop.rs (pre_land_no_expertise_runs_learner_before_readiness)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_handoff_has_no_expertise_references)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_succeeds_without_expertise_commit)
Test: src/work_task_executor.rs (pre_land_no_expertise_missing_knowledge_is_observation_only)

### B4

WHILE a pre-land Learner runs in `no-expertise` mode,
WHEN Fluent is about to launch its coder and again on a freshly re-read persisted
state immediately before Learning advances to `HandoffPending`,
THE SYSTEM SHALL require candidate `HEAD`, the Merge Candidate `candidate_commit`,
the latest Write-output commit, and every Tester and built-in reviewer context in
the final passing round to equal the reviewed Writer SHA with no staged,
unstaged, or untracked candidate change, reading only the final passing round.
An earlier passing reviewer context SHALL remain effective only when
correction-aware changed-domain invalidation proves that the later Writer did not
touch its role; that historical context is carried-forward authority rather than
a claim that its Task names the later SHA. Review admission SHALL first reopen
every affected reviewer with a missing or mismatched current context; this
preflight remains the fresh race-closing check before the Learner coder launches
or Learning advances.
Test: src/work_attempt_loop.rs (pre_land_no_expertise_requires_clean_common_sha_before_launch)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_rejects_mismatched_final_review_context)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_preserves_every_candidate_pointer)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_ignores_historical_review_contexts)
Test: src/work_attempt_loop.rs (corrective_review_roles_include_changed_domains_in_default_order)

### B4a

IF the pre-launch or fresh-read post-run no-expertise identity or cleanliness
check fails,
THEN THE SYSTEM SHALL launch no Learner coder (preflight) or advance no pointer
(postflight), persist relaunchable `Generic` failed Learning with no canonical
handoff, leave every live and persisted candidate pointer unchanged without
overwriting the live candidate, report the exact identity or cleanliness
contradiction, and withhold Merge Candidate readiness. A contradictory pointer
may already be off the reviewed Writer SHA; the fail-closed contract preserves
that evidence rather than repairing it back onto the reviewed SHA.
Test: src/work_attempt_loop.rs (pre_land_no_expertise_preflight_failure_launches_no_coder)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_postcheck_fails_closed)

### B4b

WHILE a pre-land Learner runs in `no-expertise` mode,
WHEN it settles any terminal Learning transition,
THE SYSTEM SHALL apply each transition through a fresh, lock-held mutation that
re-reads the current aggregate, accepts only this runner's exact Learning
frontier, and changes only the Learning record, so a concurrent Work-model change
during the coder run whose identity fields stay valid survives through
`HandoffPending` and `Succeeded`, a persisted identity contradiction becomes
durable relaunchable `Generic` failed Learning rather than a stale-write error, a
concurrent harder Learning transition is never overwritten, and no post-Learner
whole-aggregate write reintroduces the stale-write race.
Test: src/work_attempt_loop.rs (pre_land_no_expertise_concurrent_unrelated_field_survives_to_success)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_concurrent_pointer_contradiction_fails_closed)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_never_overwrites_a_harder_learning_frontier)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_publication_preserves_a_field_observed_after_handoff_pending)

### B5

WHERE a pre-land Learner runs in `no-expertise` mode,
THE SYSTEM SHALL run it from an isolated accepted-change snapshot, grant write
access only to its managed handoff staging surface and per-launch private
temporary directory, and deny writes to project expertise, the candidate
workspace, the live checkout, shared Git metadata, and shared temporary roots
even when the caller requested `--no-sandbox`.
Test: src/work_task_executor.rs (pre_land_no_expertise_denies_expertise_candidate_and_live_git_writes)
Test: tests/binary.rs (pre_land_no_expertise_ignores_no_sandbox_and_preserves_candidate_git)

### B5a

WHEN `fluent attempt run --runtime fargate` targets a `no-expertise` Work Item,
THE SYSTEM SHALL reject the launch before any credential lookup or injection,
Fargate bootstrap, `.fluent/Dockerfile` creation, Docker/AWS/external command,
workspace archive or upload, ECS task creation, runtime-state write, or
launch-induced Work-model mutation, and direct the operator to run the Attempt
locally on a trusted macOS host. A local host that cannot enforce the trusted
write boundary fails closed without downgrading the mode.
Test: src/fargate.rs (no_expertise_work_item_is_rejected_before_every_launch_side_effect)
Test: tests/binary.rs (fargate_no_expertise_rejection_names_local_trusted_runtime)
Test: src/work_task_executor.rs (no_expertise_requires_trusted_write_confinement)
Test: src/work_attempt_loop.rs (unsupported_no_expertise_host_preserves_candidate_and_retry)

### B6

IF the no-expertise invocation ledger contains any candidate Git mutation —
including an expertise-only change or one a later invocation reverted — or
canonical handoff publication fails after a clean result with no typed failure
classification,
THEN THE SYSTEM SHALL dispose of the isolated workspace, persist relaunchable
`Generic` failed Learning with no handoff reference, leave every pointer at the
reviewed Writer SHA, and withhold Merge Candidate readiness.
Test: src/work_attempt_loop.rs (pre_land_no_expertise_accounts_every_invocation_return)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_rejects_every_git_mutation)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_handoff_failure_preserves_reviewed_sha)

### B6a

WHILE a pre-land Learner runs in `no-expertise` mode,
WHEN it settles canonical publication and failure across its two lock-held phases,
THE SYSTEM SHALL settle through `prepare_no_expertise_handoff`, which rechecks the
reviewed identity and advances Learning `InProgress` to `HandoffPending` or a typed
`Failed`, and then `publish_no_expertise_handoff`, which reacquires the lock,
re-runs the final identity check, publishes the canonical handoff, and settles
`HandoffPending` to `Succeeded` or a typed `Failed`; SHALL hold a reviewed-identity
transition guard over `HandoffPending` and `Succeeded` while keeping a change made
during `InProgress` postflight-detectable and a relaunchable `Failed` record
repairable; SHALL recover both transaction-journal outcomes — rerunning only the
Learner when no terminal journal became durable and finishing the exact durable
journal to `Succeeded` without rerunning otherwise; and SHALL settle integrity,
cleanliness, and candidate-mutation contradictions as `Generic` while a publisher
failure preserves the `LearningFailureKind` derived from its concrete cause
(`EvidencePending` for a coder-already-ran supervision fault, then `TranscriptPump`
for a transcript-pump publication fault), so not every handoff-publication failure
is `Generic`.
Test: src/work_attempt_loop.rs (pre_land_no_expertise_real_final_publication_pointer_contradictions_fail_closed)
Test: src/work_attempt_loop.rs (pre_land_no_expertise_settlement_errors_preserve_exact_primary_diagnostics)
Test: src/work_model.rs (no_expertise_frozen_identity_guard_covers_every_supported_write_and_pointer)
Test: src/work_model.rs (no_expertise_unexposed_identity_repair_uses_both_write_apis)

### B7

WHILE a pre-land no-expertise Learning record is relaunchable,
WHEN `fluent attempt run` is invoked again,
THE SYSTEM SHALL retry only Learner in `no-expertise` mode without rerunning
Writer, Tester, or reviewers, preserving the stored mode.
Test: tests/binary.rs (pre_land_no_expertise_retry_runs_only_learner_and_preserves_mode)

### B8

WHEN an already-merged candidate's Learner is retried,
THE SYSTEM SHALL use post-land handoff-only execution regardless of the Work
Item's stored Learner mode.
Test: src/work_attempt_loop.rs (post_land_retry_is_handoff_only_for_every_stored_mode)

### B9

WHEN the confirmed Work Item contract requires one reviewed Writer SHA to survive
Learner unchanged, the bundled Fluent `plan-execution` skill SHALL create that
Work Item with `--learner-mode no-expertise`; otherwise it SHALL omit
`--learner-mode` so the Work Item uses the default `capture` policy.
Test: src/content.rs (bundled_plan_execution_selects_no_expertise_for_common_sha_work)
Test: src/content.rs (bundled_plan_execution_preserves_capture_by_default)

### B10

WHEN an existing external caller invokes public `run_learner` through
`LearnerRunInputs`,
THE SYSTEM SHALL preserve the current `handoff_only: false` capture behavior and
`handoff_only: true` post-land behavior without exposing a contradictory public
no-expertise switch.
Test: tests/public_api.rs (learner_run_inputs_handoff_only_surface_remains_source_compatible)
Test: src/work_task_executor.rs (public_handoff_only_boolean_maps_to_existing_modes)

### B11

WHEN the Learner builds the coder's system and user prompts in any mode,
THE SYSTEM SHALL construct them through one shared production builder that both the
launch route and the tests use, covering the initial audit and the bounded
schema-repair invocation in capture, no-expertise, and post-land handoff-only modes,
so a tested prompt can never drift from what the coder actually receives.
Test: src/work_task_executor.rs (learner_production_launch_prompts_cover_every_mode_and_invocation)

### B12

IF the Learner's production system prompt or user prompt cannot be resolved,
rendered, or fully substituted,
THEN THE SYSTEM SHALL return the prompt error before constructing or launching the
coder, so no coder is ever built or launched from a mis-rendered prompt.
Test: src/work_task_executor.rs (learner_production_prompt_failures_prevent_coder_construction)

### B13

WHEN an operator prepares a Work Item whose Learner mode matters, the bundled
plan-execution guidance SHALL select the Learner mode before any create command and
present the capture and no-expertise create forms as mutually exclusive branches,
because `fluent work-item create` fixes the mode irreversibly.
Test: src/content.rs (bundled_plan_execution_selects_mode_before_creating)
Test: src/content.rs (living_behaviors_cover_no_expertise_prompt_and_create_order_contracts)

### B14

WHEN a fresh, unmerged `no-expertise` `Succeeded` candidate lands,
THE SYSTEM SHALL land it through an identity-preserving exact-SHA route that skips
rebase and provenance regeneration, runs `check-pre-merge` — never `fix-pre-merge`
— only in a disposable exact-SHA worktree, and fast-forwards the target to exactly
the reviewed Writer SHA. Successful removal of that disposable worktree is a land
precondition: a passing check whose worktree cannot be removed fails the land
before the second precondition check or any target Git mutation and retains at
most the isolated worktree. The route enforces the same target-worktree
cleanliness policy capture landing uses, both before any side effect and again
immediately before the fast-forward. Both routes schedule the optional post-merge
review through one shared follow-up-processing coordinator only after the landed
follow-up result is durably recorded, and schedule nothing while returning the
successful landed outcome when that persistence is unknown. An already-merged
`no-expertise` record derives its reviewed Writer SHA independently of Learning
status and fails closed when the persisted `merged_commit` is absent or differs
from it.
Test: src/work_merge_executor.rs (no_expertise_passing_check_cleanup_failure_blocks_land)
Test: src/work_merge_executor.rs (no_expertise_land_records_follow_up_outcome_before_post_merge_review)
Test: src/work_merge_executor.rs (no_expertise_merged_resume_requires_reviewed_sha_for_every_learning_state)
Test: src/work_merge_executor.rs (no_expertise_dirty_target_fails_before_side_effects)
Test: src/work_merge_executor.rs (no_expertise_land_rechecks_target_cleanliness_after_isolated_check)

### B15

WHEN Fluent launches a Learner provider in any execution mode, THE SYSTEM SHALL
pass the explicit Learner task kind, resolved mode, and exact managed draft path
through the provider environment after applying trusted environment filtering.
Test: src/work_task_executor.rs (learner_production_launch_prompts_cover_every_mode_and_invocation)

## Corrective classification and Work authorization

### B1

WHEN the Learner is asked to evaluate whether a follow-up is corrective,
THE SYSTEM SHALL instruct it to propose corrective status only when it
judges that an existing authoritative behavior or convention is being
violated, the evidence is concrete, scope is bounded, verification is
deterministic, and no consequential product, interface, architecture,
security, or permission decision remains unresolved.
Test: src/work_task_executor.rs (learner_prompt_requires_bounded_authoritative_corrective_context)

## Merge provenance compatibility

### B1

WHEN rebase changes a candidate tip,
THE SYSTEM SHALL update the Merge Candidate commit, every completed Write
Task output commit, and only the Attempt artifact references that
represent those Write commits; it SHALL preserve learner handoff, Tester,
reviewer, and other non-Write artifact references.
Test: src/work_merge_executor.rs (regenerate_provenance_updates_write_commit_artifacts_only)
Test: src/work_merge_executor.rs (regenerate_provenance_preserves_learner_handoff_reference)

---

## Primary-flow command guidance

### B1

WHEN a primary-flow command completes at a state transition or terminal outcome,
THE SYSTEM SHALL print, after the result, a next-action line naming the specific `fluent`
command to run next for that state.
Test: tests/binary.rs (work_item_create_output_names_attempt_create_next)
Test: tests/binary.rs (attempt_create_output_names_attempt_run_next)
For evidence recovery without an eligible command, THE SYSTEM SHALL report the
missing proof or authorization instead of inventing a next operation.
Test: src/guidance.rs (missing_check_proof_does_not_offer_generic_resume)

### B2

WHEN `fluent attempt run` reports a ready Merge Candidate,
THE SYSTEM SHALL print next actions that name
`merge-candidate show <work-item-id> <merge-candidate-id>` and
`merge-candidate land <work-item-id> <merge-candidate-id>` with the
authoritative Work Item and Merge Candidate identifiers.
Test: tests/binary.rs (attempt_run_output_names_executable_merge_candidate_commands)

### B2a

WHEN `fluent attempt run` plans a follow-up Writer round,
THE SYSTEM SHALL print `attempt run` as the next action.
Test: src/guidance.rs (after_attempt_run_planned_write_round_conveys_iteration)

### B2b

WHEN `fluent attempt run` reports a relaunchable Learner failure,
THE SYSTEM SHALL print `attempt run` as the next action.
Test: src/guidance.rs (after_attempt_run_relaunchable_learner_failure_names_retry)

### B2c

WHEN `fluent attempt run` reports a non-relaunchable Learner evidence
failure,
THE SYSTEM SHALL name Work Item inspection as the next action and SHALL
NOT name rerun or land.
Test: src/guidance.rs (after_attempt_run_non_relaunchable_learner_failure_names_human_inspection)

### B2d

WHEN `fluent attempt run` reports a failed Attempt,
THE SYSTEM SHALL name the recovery or inspection step as the next action.
Test: tests/binary.rs (attempt_run_output_names_next_action_for_failed)

### B2e

WHEN `fluent attempt run` reports a review-only completion,
THE SYSTEM SHALL name the passing review results to inspect as the next
action.
Test: src/guidance.rs (after_attempt_run_review_only_complete_names_artifact)

### B2f

WHEN `fluent attempt run` reports a review-only failure,
THE SYSTEM SHALL name the failed review results to inspect as the next
action.
Test: src/guidance.rs (after_attempt_run_review_only_failed_names_artifact)

### B3

WHEN an Attempt is `needs-user` for an auth pause,
THE SYSTEM SHALL print the coder-specific re-authentication step (`claude /login` for
Claude, `codex login` for Codex, and a coder-agnostic phrase when the coder is unknown),
then `fluent attempt run`.
Test: tests/binary.rs (needs_user_auth_output_names_reauth_and_attempt_run)
Test: tests/binary.rs (needs_user_auth_output_is_coder_aware)
Test: src/guidance.rs (after_attempt_run_needs_user_auth_is_coder_aware)
Test: src/guidance.rs (coder_reauth_step_is_coder_specific)

### B4

WHEN `fluent status` or `fluent work-item list` runs with no Work Items,
THE SYSTEM SHALL print a planning-stage primer directing the agent to capture a brief,
then behaviors, approach, and plan, then `work-item create`.
Test: tests/binary.rs (empty_status_output_primes_planning_stages)

### B5

WHEN a printed next-action involves a fluent skill stage,
THE SYSTEM SHALL include a drift-pointer naming the fluent skill stage to re-read.
Test: tests/binary.rs (planning_primer_points_to_fluent_skill)

### B6

WHERE quiet output is requested (FLUENT_QUIET set to a truthy value),
THE SYSTEM SHALL print only the command result, omitting the next-action and drift-pointer
lines.
Test: tests/binary.rs (quiet_mode_omits_next_action_hints)

### B7

WHEN a primary-flow command prints a next-action line,
THE SYSTEM SHALL keep the command result itself readable and unchanged in meaning, with
the next-action additive and visually distinct (not interleaved into the result).
Test: tests/binary.rs (next_action_is_appended_not_interleaved_into_result)

### B8

WHEN an Attempt is `needs-user` for a non-auth pause,
THE SYSTEM SHALL print the applicable recovery action and handoff. For evidence
recovery, it SHALL use the eligible evidence operation or name missing proof;
other non-auth pauses retain the handoff followed by `fluent attempt run`.
Test: tests/binary.rs (needs_user_generic_output_names_handoff_file)
Test: src/guidance.rs (after_attempt_run_needs_user_generic_names_handoff_file)
Test: tests/binary.rs (evidence_recovery_guidance_matches_foreground_and_queue_admission)

### B9

WHEN `fluent status` or `fluent work-item show <id>` runs with at least one actionable
Work Item on the board,
THE SYSTEM SHALL print a next-action line naming the `fluent` command for the
most-actionable Work Item, choosing it by priority needs-user > merge-ready >
learner-not-ready > task-ready.
Test: tests/binary.rs (status_names_next_action_for_actionable_state)
Test: src/guidance.rs (status_next_action_prioritizes_needs_user_over_merge_and_task_ready)
Test: src/guidance.rs (status_next_action_names_merge_ready_over_task_ready)
Test: src/guidance.rs (status_next_action_names_learner_retry_over_task_ready)

### B9a

WHEN `fluent status` or `fluent work-item show <id>` selects a
`merge-ready` row for its next action,
THE SYSTEM SHALL render
`fluent merge-candidate show <work-item-id> <merge-candidate-id>` with
the row's authoritative Work Item and Merge Candidate identifiers.
Test: tests/binary.rs (status_names_executable_merge_candidate_show)
Test: src/guidance.rs (status_next_action_names_concrete_merge_candidate)

### B9b

WHEN `fluent status` or `fluent work-item show <id>` finds no actionable
Work Item,
THE SYSTEM SHALL print no next-action line.
Test: src/guidance.rs (status_next_action_is_none_for_only_learner_blocked_work)
Test: src/guidance.rs (status_next_action_is_none_when_nothing_actionable)

### B9c

WHEN a `learner-blocked` row appears with another actionable Work Item,
THE SYSTEM SHALL keep the blocked row visible without letting it suppress
the other Work Item's next action.
Test: src/guidance.rs (status_next_action_ignores_learner_blocked_when_merge_ready_exists)

### B10

WHEN a read or side command completes — a populated `fluent work-item list`,
`fluent observation create`, or `fluent cleanup` —
THE SYSTEM SHALL print a next-action line pointing at the natural follow-up
(`fluent status` or `fluent work-item show` for the list and cleanup,
`fluent observation list` for a recorded observation).
Test: tests/binary.rs (side_commands_name_next_action)
Test: src/guidance.rs (side_command_hints_name_next_step)

### B11

WHEN a review-only Attempt completes or fails and the latest review artifact path is
known,
THE SYSTEM SHALL name that artifact path in the next-action line — pointing at the passing
verdicts to read, or at the failing verdicts to inspect before addressing them — and SHALL
fall back to a path-less phrasing when no artifact path is available.
Test: src/guidance.rs (after_attempt_run_review_only_complete_names_artifact)
Test: src/guidance.rs (after_attempt_run_review_only_failed_names_artifact)
Test: src/guidance.rs (after_attempt_run_review_only_complete_names_verdicts)

### B12

WHEN the latest code-producing Attempt has passed review and has a pending Merge
Candidate but its Learning state is absent, in progress, handoff-pending, or
failed with a relaunchable disposition,
THE SYSTEM SHALL label the Work Item `learner-not-ready` rather than
`merge-ready`, direct the operator to `fluent attempt run` to run or retry the
Learner, and SHALL NOT direct the operator to land the candidate.
Test: src/work_status.rs (summarize_passed_attempt_without_learning_is_not_merge_ready)
Test: src/work_status.rs (summarize_passed_attempt_with_running_learning_is_not_merge_ready)
Test: src/work_status.rs (summarize_passed_attempt_with_handoff_pending_learning_is_not_merge_ready)
Test: src/work_status.rs (summarize_passed_attempt_with_failed_learning_is_not_merge_ready)
Test: src/guidance.rs (next_action_for_learner_not_ready_names_attempt_run_without_land)

### B13

WHEN the latest code-producing Attempt has passed review and has a pending Merge
Candidate whose Learner failed with the non-relaunchable `EvidencePending`
disposition,
THE SYSTEM SHALL label the Work Item `learner-blocked`, print one Work Item
inspection hint from the originating Attempt outcome, and SHALL NOT direct the
operator to rerun the Learner or land the candidate. Later status and Work Item
show commands SHALL emit no next action for that row, so they neither self-loop
on inspection nor suppress another actionable Work Item.
Test: src/work_status.rs (summarize_passed_attempt_with_non_relaunchable_learning_is_blocked)
Test: src/guidance.rs (after_attempt_run_non_relaunchable_learner_failure_names_human_inspection)
Test: src/guidance.rs (next_action_for_learner_blocked_is_none_to_avoid_inspection_loop)
Test: src/guidance.rs (status_next_action_ignores_learner_blocked_when_merge_ready_exists)
Test: src/guidance.rs (status_next_action_is_none_for_only_learner_blocked_work)

### B14

WHEN `fluent merge-candidate land` completes successfully,
THE SYSTEM SHALL print `fluent cleanup` as the next command without a
positional Work Item or Merge Candidate identifier, preserving cleanup's
default dry run.
Test: src/guidance.rs (after_merge_candidate_land_names_global_cleanup)
Test: tests/binary.rs (successful_land_names_executable_cleanup)

## Fluent state tracking

### B1

WHEN `fluent init` runs (whether it initializes a fresh `.fluent/` or finds an
existing one),
THE SYSTEM SHALL print a notice that fluent keeps its learned project notes and
test config in `.fluent/` and commits them alongside the user's changes.
Test: tests/binary.rs (init_output_notes_fluent_tracks_its_state)

## Autonomous Codex worker boundary

### B1

WHEN Fluent launches an autonomous Codex Writer, Reviewer, Learner, or rebase
worker, THE SYSTEM SHALL disable hooks, ignore user configuration, and preserve
the Attempt's selected model and effort.
Test: src/coder.rs (autonomous_codex_command_isolated_from_user_hooks_config_and_sessions)

### B2

WHEN Fluent launches an autonomous Codex worker, THE SYSTEM SHALL avoid session
persistence for Reviewer, Learner, and rebase launches and retain only the
rollout material required by an approved pre-review Writer continuation.
Test: src/coder.rs (fresh_writer_codex_command_persists_its_session)
Test: src/codex_worker.rs (writer_session_snapshot_round_trips_without_auth_or_configuration)

### B3

WHEN Fluent launches an autonomous Codex worker, THE SYSTEM SHALL use a private
temporary `CODEX_HOME` and grant the sandbox no access to the interactive Codex
home.
Test: src/os.rs (autonomous_codex_profile_grants_only_worker_home)

### B4

WHEN Fluent launches Codex interactively, THE SYSTEM SHALL continue using the
user's normal configuration, hooks, and Codex home.
Test: src/coder.rs (interactive_codex_command_preserves_user_configuration)
Test: src/os.rs (interactive_codex_profile_preserves_source_home_access)

### B5

WHEN Fluent prepares an autonomous Codex worker, THE SYSTEM SHALL stage only
the supported authentication state from the interactive Codex home; skills may
enter the private home only through explicit `codex.skill-roots`.
Test: src/codex_worker.rs (worker_home_stages_only_auth_from_effective_codex_home)

### B6

WHEN Fluent prepares an autonomous Codex worker, THE SYSTEM SHALL run `codex
login status` without making a model request.
Test: src/codex_worker.rs (login_status_preflight_accepts_authenticated_worker_home)

### B7

WHEN Fluent checks an autonomous Codex worker's login status, THE SYSTEM SHALL
run the check from a working directory that the retained production profile can
read.
Test: src/os.rs (codex_launcher_preflight_uses_a_profile_readable_working_directory)

### B8

WHEN an autonomous Codex worker finishes or fails to launch, THE SYSTEM SHALL
remove its private home and copied authentication state.
Test: src/codex_worker.rs (worker_home_is_removed_after_launch_scope)
Test: tests/binary.rs (autonomous_codex_writer_uses_and_removes_worker_home)

### B9

IF Codex authentication cannot be prepared before a Writer or Reviewer starts,
THEN THE SYSTEM SHALL pause the same planned Task and Attempt without reserving
execution or consuming a Writer round, and tell the user to run `codex login`.
Test: tests/binary.rs (codex_auth_failure_pauses_planned_task_before_reservation)
Test: tests/binary.rs (codex_auth_failure_pauses_planned_reviewer_before_reservation)

### B10

WHEN `fluent attempt run` resumes an Attempt paused by Codex authentication
preflight after authentication succeeds, THE SYSTEM SHALL reopen and run the
same Task through its stored coder mapping without adding a Writer round.
Test: tests/binary.rs (codex_auth_resume_reopens_same_task_without_new_writer_round)

### B11

IF Codex authentication cannot be prepared before a Learner or rebase launch,
THEN THE SYSTEM SHALL record neither a Learner run nor an executing Rebase Task,
and tell the user to run `codex login`.
Test: tests/binary.rs (learner_codex_auth_preflight_precedes_run_reservation)
Test: tests/binary.rs (rebase_codex_auth_preflight_precedes_task_creation)

### B12

WHEN user configuration supplies absolute `codex.skill-roots` and project
configuration supplies project-contained `codex.skill-roots`, THE SYSTEM SHALL
combine and canonicalize those roots for the autonomous Codex worker.
Test: src/config.rs (codex_skill_roots_combine_user_external_and_project_local_paths)

### B13

IF a configured Codex skill root is relative in user configuration, escapes the
project in project configuration, is missing, is not a directory, or is a
symlink, THEN THE SYSTEM SHALL fail before reserving an autonomous role.
Test: src/config.rs (user_codex_skill_root_must_be_absolute)
Test: src/config.rs (project_codex_skill_root_cannot_escape_the_project)
Test: src/config.rs (configured_codex_skill_root_must_exist)
Test: src/config.rs (configured_codex_skill_root_must_be_a_directory)
Test: src/config.rs (configured_codex_skill_root_may_not_be_a_symlink)

### B14

WHEN Fluent prepares configured skills for an autonomous Codex worker, THE
SYSTEM SHALL copy one stable snapshot into the private `$CODEX_HOME/skills`,
reject symlinks inside a copied tree and duplicate top-level skill names, and
leave unrelated root files and later source changes outside that snapshot.
Test: src/codex_worker.rs (configured_skills_are_snapshotted_into_the_private_codex_home)
Test: src/codex_worker.rs (configured_skill_tree_rejects_symlinks)
Test: src/codex_worker.rs (duplicate_configured_skill_names_fail_closed)

### B15

WHERE a project-root `AGENTS.md` exists and an autonomous Reviewer runs from
its artifact directory, THE SYSTEM SHALL grant that exact instruction file as
read-only input without granting adjacent project files. Behaviors B39a and
B39b govern a symlinked instruction file.
Test: src/work_task_executor.rs (reviewer_project_instruction_grant_follows_an_in_project_file_symlink)

### B16

WHEN the selected `codex` command is an official npm Node wrapper with a
matching platform package, THE SYSTEM SHALL retain the wrapper for readiness
checks and use the version-verified packaged native executable for autonomous
Writer, Reviewer, Learner, and rebase work.
Test: src/codex_worker.rs (resolved_launcher_retains_wrapper_and_uses_verified_native_for_execution)

### B17

IF an official npm Codex wrapper has no matching executable native package, its
native package version differs from the wrapper package, or its native binary
resolves outside the verified package, THEN THE SYSTEM SHALL fail preparation
before reserving autonomous work.
Test: src/codex_worker.rs (resolved_packaged_wrapper_rejects_missing_native_runtime)
Test: src/codex_worker.rs (resolved_packaged_wrapper_rejects_mismatched_native_version)
Test: src/codex_worker.rs (resolved_packaged_wrapper_rejects_native_symlink_escape)

## Reviewer build-cache bounds

### B1

WHEN Fluent prepares a tests Reviewer Task, THE SYSTEM SHALL retire
candidate-keyed caches no longer referenced by nonterminal Work before
admitting a warm cache, while preserving a cache referenced by any nonterminal
Work Item.
Test: src/work_task_executor.rs (candidate_cache_retires_once_work_is_terminal)
Test: src/work_task_executor.rs (candidate_cache_remains_while_work_is_nonterminal)

### B2

WHEN the prospective managed reviewer cache fits the configured project budget
and host free-space floor, THE SYSTEM SHALL make the candidate's canonical
build-cache directories available in one cache keyed by the exact candidate
commit and SHALL reuse that cache for parallel review work.
Test: src/work_task_executor.rs (reviewer_cache_admission_warms_within_project_budget)
Test: src/work_task_executor.rs (parallel_reviewers_share_one_cache_and_create_no_private_targets)

### B3

IF cache configuration is invalid, accounting or free-space inspection fails,
or a prospective warm cache exceeds either limit, THEN THE SYSTEM SHALL name
the Reviewer and reason, start it cold, and continue the Attempt.
Test: src/work_task_executor.rs (reviewer_cache_budget_starts_cold_without_pausing_review)
Test: src/work_task_executor.rs (reviewer_cache_free_space_floor_starts_cold_without_pausing_review)
Test: src/work_task_executor.rs (invalid_reviewer_cache_limits_start_cold_and_name_config)
Test: src/work_task_executor.rs (reviewer_cache_accounting_failure_starts_cold)
Test: src/work_task_executor.rs (reviewer_cache_free_space_failure_starts_cold)

### B4

WHEN a Reviewer Task settles, THE SYSTEM SHALL write structured artifact byte
accounting, warn when the artifact exceeds its size threshold or contains a
managed build cache, and preserve review evidence, logs, transcripts, and
noncanonical hook output without creating a private cache there.
Test: src/work_task_executor.rs (prepare_pre_review_reclaims_only_canonical_cache_dirs)
Test: src/work_task_executor.rs (failed_prepare_pre_review_removes_managed_cache)
Test: src/work_task_executor.rs (review_artifact_usage_records_bytes_and_warns_on_private_cache)

## Writer dependency-cache confinement

### B1

WHEN Fluent launches initial, continuation, or corrective Writers for one exact
Attempt, THE SYSTEM SHALL give every round the same Fluent-owned `CARGO_HOME`.
Test: src/writer_cache.rs (writer_rounds_reuse_exact_attempt_cache_without_cross_attempt_sharing)
Test: src/work_task_executor.rs (writer_launch_uses_private_dependency_cache)

### B2

WHEN Fluent launches a Writer for another Attempt, THE SYSTEM SHALL assign
another `CARGO_HOME` path.
Test: src/writer_cache.rs (writer_rounds_reuse_exact_attempt_cache_without_cross_attempt_sharing)

### B3

WHEN Fluent prepares a Writer dependency cache, THE SYSTEM SHALL keep it under
`.fluent/work/cache/writers/<work-item-id>/<attempt-id>/` rather than a durable
artifact or the operator's Cargo home.
Test: src/writer_cache.rs (writer_dependency_cache_stays_outside_durable_artifacts)
Test: src/work_task_executor.rs (writer_sandbox_confines_dependency_writes_to_the_exact_attempt_cache)

### B4

WHEN the host has a canonical Cargo dependency cache, THE SYSTEM SHALL admit
only its `registry/` and `git/` trees and SHALL exclude Cargo credentials,
configuration, and binaries.
Test: src/writer_cache.rs (preparation_admits_only_dependency_trees_from_the_canonical_cargo_home)

### B5

WHEN a Writer changes its private dependency cache, THE SYSTEM SHALL leave the
admitted host cache unchanged.
Test: src/writer_cache.rs (private_dependency_writes_do_not_mutate_the_admitted_source)

### B6

WHEN admitted dependencies were previously fetched, THE SYSTEM SHALL permit a
Writer's focused Cargo verification to pass offline after the original
dependency source becomes unavailable.
Test: src/writer_cache.rs (an_admitted_git_dependency_builds_offline_from_the_private_cache)

### B7

IF an existing managed cache component is a symlink, or a copied dependency
symlink resolves outside the private cache, THEN THE SYSTEM SHALL reject cache
preparation before launching the Writer.
Test: src/writer_cache.rs (preflight_rejects_a_symlinked_managed_cache_ancestor)
Test: src/writer_cache.rs (preparation_rejects_a_dependency_symlink_that_escapes_the_private_cache)
Test: src/writer_cache.rs (preparation_rejects_a_cache_leaf_symlink_before_writing_through_it)
Test: src/work_task_executor.rs (writer_cache_symlink_rejection_leaves_the_task_unstarted)

### B8

WHEN Fluent reclaims Writer dependency caches, THE SYSTEM SHALL preserve an
exact Work Item/Attempt cache referenced by nonterminal Work.
Test: src/writer_cache.rs (reclamation_preserves_a_cache_referenced_by_nonterminal_work)

### B9

WHEN Fluent reclaims Writer dependency caches after Work becomes terminal, THE
SYSTEM SHALL retire that Work Item/Attempt cache.
Test: src/writer_cache.rs (reclamation_retires_a_cache_after_work_becomes_terminal)

## Generated commit wording

### B1

WHEN `fix-pre-merge` changes candidate files and Fluent creates the
corresponding commit,
THE SYSTEM SHALL persist a nonempty commit message containing exactly one
subject, a blank separator, and an explanatory body.
Test: src/work_merge_executor.rs (commit_autofix_writes_approved_message)

### B2

WHEN `fix-pre-merge` changes candidate files and Fluent creates the
corresponding commit,
THE SYSTEM SHALL persist `Conform code to project standards` as the subject
and `Apply project formatting fixes.` as its body.
Test: src/work_merge_executor.rs (commit_autofix_writes_approved_message)

## Host-action ownership

### B1

WHEN Fluent evaluates Writer completion, THE SYSTEM SHALL require only
Writer-owned obligations while retaining host verification and later workflow
stages as separate requirements.
Test: src/work_attempt_loop.rs (writer_completion_excludes_host_and_later_stage_work)

### B2

WHEN completed Writer work leaves a host-only action next, THE SYSTEM SHALL
pause at `needs-user` with the exact candidate, action, and required evidence
before launching another Writer, including after earlier reviews.
Test: src/work_attempt_loop.rs (pending_host_action_pauses_before_another_writer)
Test: tests/binary.rs (writer_completion_plan_preserves_host_only_handoff)

### B3

WHILE required host verification lacks accepted passing evidence for the exact
candidate, THE SYSTEM SHALL keep the candidate ineligible for readiness.
Test: src/host_evidence.rs (pending_host_verification_blocks_readiness)

### B4

WHEN an operator previews responsibility for a saved mixed Plan obligation,
THE SYSTEM SHALL show the exact candidate and complete proposed split without
adopting it until a fresh explicit approval is recorded.
For a legacy row without a stored command, a host split SHALL use the one exact
verification command in the original approved text, and SHALL remain invalid
when that text supplies no command or ambiguous commands.
Test: tests/binary.rs (legacy_ownership_adoption_requires_approval)
Test: tests/binary.rs (legacy_ownership_adoption_recovers_missing_command)
Test: tests/binary.rs (legacy_ownership_adoption_rejects_unresolved_command)
Test: tests/binary.rs (legacy_ownership_adoption_preserves_stored_command)
Test: tests/binary.rs (legacy_ownership_adoption_rejects_stale_state)
Test: tests/binary.rs (legacy_ownership_adoption_exact_retry_preserves_history)
Test: tests/binary.rs (legacy_ownership_old_reader_refuses_mutation)

### B5

WHEN an operator authorizes an Approach revision after ownership is assigned,
THE SYSTEM SHALL retain the non-Approach contract, progress, and host-evidence
history, and materialize the corrective Writer with the revised Approach.
Exact retries SHALL reuse the recorded revision only while its audit and source
completion bytes remain unchanged. After the corrective Writer succeeds, THE
SYSTEM SHALL retain the settled historical review, require fresh host evidence
for its new candidate, and allow foreground and queued continuation.
Test: tests/binary.rs (legacy_ownership_survives_approach_recovery)
Test: tests/binary.rs (explicit_plan_survives_approach_recovery)
Test: src/planning_approach_recovery.rs (replays_reference_approach_reference_history)

## Failed-review round-cap Approach revision

### B1

WHEN an operator authorizes a replacement Approach for a failed-review
`round-cap` pause, and a matching round extension leaves an unused Writer round,
THE SYSTEM SHALL accept the revision even when Architecture review passed,
subject to the existing recovery safety checks.
Test: tests/binary.rs (round_cap_approach_revision_accepts_extended_failed_review)

### B2

IF no matching extension provides an unused Writer round,
THEN THE SYSTEM SHALL reject this revision request without changing planning,
the Attempt, or its round limit.
Test: tests/binary.rs (round_cap_approach_revision_requires_extension)

### B3

WHEN Fluent accepts the revision,
THE SYSTEM SHALL save an immutable audit containing the authorization, old and
replacement Approach, candidate commit, and exact supporting review identities
and content digests.
Test: tests/binary.rs (round_cap_approach_revision_records_audit)

### B4

WHEN Fluent publishes the revision,
THE SYSTEM SHALL expose the saved Approach and Writer completion requirements
as one consistent revision, never a partially updated combination.
Test: tests/binary.rs (round_cap_approach_revision_updates_planning_and_contract_together)

### B5

WHEN Fluent accepts the revision,
THE SYSTEM SHALL preserve non-Approach requirements, assigned responsibilities,
original completion reports, accepted host evidence, review verdicts, and
completed Task history.
Test: tests/binary.rs (round_cap_approach_revision_preserves_authority_and_history)

### B6

WHEN Fluent accepts the revision,
THE SYSTEM SHALL prepare exactly one corrective Writer under the revised
Approach, using one already-authorized round without starting execution.
Test: tests/binary.rs (round_cap_approach_revision_prepares_one_writer)

### B7

WHEN the identical revision request repeats and its recorded authorization and
source evidence remain valid,
THE SYSTEM SHALL return the existing revision and Writer without creating
another Task or consuming another round.
Test: tests/binary.rs (round_cap_approach_revision_retry_is_idempotent)

### B8

IF revision publication is interrupted and its bound inputs remain valid,
THEN THE SYSTEM SHALL allow the same request to complete that revision without
duplicating its audit or corrective Writer.
Test: tests/binary.rs (round_cap_approach_revision_recovers_interrupted_publication)

This path applies only to a failed-review round-cap pause. Existing uncertain-
Architecture recovery and historical replay retain their prior admission rules;
failed-Tester and unrelated pause types gain no new Approach-revision
eligibility. Acceptance and resume continue to require a clean exact candidate,
bound evidence, cancellation precedence, live-owner exclusion, and supported
stored formats. Recording the revision does not launch a worker. After the
corrective Writer completes, the Attempt must still pass its separate
host-evidence, candidate, Review, final-Tester, and Learning gates. The Writer
owns only its Writer obligations; those host and later Attempt gates remain
separate.

## Writer recovery instructions

### B1

WHEN a Writer starts or resumes under an approved recovery, THE SYSTEM SHALL
deliver its complete current authorized instructions and validated evidence
references through the context actually supplied to that Writer, including
host-evidence source corrections and the other supported Writer recovery paths.
The applicable no-change declaration and completion contract remain available.
Full content may use a permitted readable reference within the current prompt
and access limits.
Test: tests/binary.rs (writer_recovery_context_delivers_recorded_resolution)
Test: tests/binary.rs (declared_sandbox_input_reads_exact_writer_recovery)
Test: tests/binary.rs (classified_writer_receives_current_authorized_instructions)
Test: tests/binary.rs (writer_completion_plan_context_preserves_obligations)
Test: tests/binary.rs (writer_completion_plan_real_sandbox_inputs)

### B2

WHEN Fluent prepares a Writer with pending obligations, THE SYSTEM SHALL expose
their original identities and assigned findings in current context independently
of stale progress checkboxes and current-candidate verification, leaving external
responsibilities pending and uncredited.
Test: tests/binary.rs (writer_recovery_context_preserves_pending_obligations)

### B3

IF required recovery instructions or evidence cannot be validated or delivered
within the permitted access and context limits, THEN THE SYSTEM SHALL reject
Writer launch with the specific blocking condition. Historical metadata text
alone SHALL NOT authorize rewriting the recorded candidate's ancestry, and
invalid input cannot be repaired by widening worker access.
Test: tests/binary.rs (writer_recovery_context_rejects_invalid_delivery)
Test: tests/binary.rs (classified_writer_rejects_invalid_required_inputs_before_launch)
Test: tests/binary.rs (writer_completion_plan_rejects_changed_authority)

### B4

WHEN an approved source correction follows an earlier request for more host
evidence, THE SYSTEM SHALL identify the source correction as the current
assigned action in the Writer context.
Test: tests/binary.rs (classified_writer_context_identifies_current_assignment)

### B5

WHEN a Writer receives an approved source correction, THE SYSTEM SHALL retain
the earlier evidence, classification authorization, exact candidate bindings,
and remaining verification gates.
Test: tests/binary.rs (classified_writer_context_identifies_current_assignment)

## Interrupted candidate restoration

### B1

WHEN the operator authorizes restoration of an eligible settled interrupted
Writer, THE SYSTEM SHALL restore the latest completed Writer's exact candidate
while retaining the rejected history and a candidate-bound recovery audit.
It SHALL preserve completed Tasks and evidence, pending obligations, the
unfinished Writer, and active cancellation.
Test: tests/binary.rs (interrupted_writer_candidate_restore_preserves_history)
Test: tests/binary.rs (interrupted_writer_candidate_restore_preserves_inherited_assignment)
Test: tests/binary.rs (interrupted_writer_candidate_restore_preserves_frozen_legacy_context)
Test: tests/binary.rs (interrupted_writer_candidate_restore_preserves_recovered_context)

### B2

IF the candidate, workspace, source, evidence, authorization, or settled ownership
does not match the restoration preconditions, THEN THE SYSTEM SHALL reject
restoration without changing history or Work state.
Test: tests/binary.rs (interrupted_writer_candidate_restore_rejects_drift)
Test: tests/binary.rs (interrupted_writer_candidate_restore_rejects_aliased_owner)
Test: tests/binary.rs (interrupted_writer_candidate_restore_rejects_broken_assignment_lineage)
Test: tests/binary.rs (interrupted_writer_candidate_restore_rejects_frozen_context_drift)
Test: tests/binary.rs (interrupted_writer_candidate_restore_rejects_context_audit_drift)
Test: tests/binary.rs (interrupted_writer_candidate_restore_rejects_symbolic_branch_substitution)

### B3

WHEN the identical restoration is retried after interruption or completion,
THE SYSTEM SHALL reconcile or return the same operation without discarding
intervening state or duplicating recovery effects. A later explicit resume SHALL
retry the original unfinished Writer with its valid recorded instructions.
Test: tests/binary.rs (interrupted_writer_candidate_restore_replays_exactly)
Test: tests/binary.rs (interrupted_writer_candidate_restore_replays_legacy_intent)

## Writer continuation recovery

### B1

The canonical stagnation guarantee is Pre-review completion gate:B8 in
[Pre-review completion gate](#pre-review-completion-gate), including corrective
continuations after review. Host-action ownership:B2 requires an immediate host
handoff when that is the next action, without waiting for the stagnation guard.

### B2

WHEN an interrupted Writer has stopped without output, all Writer obligations
are complete, and the clean unchanged candidate awaits an approved host check,
THE SYSTEM SHALL admit fresh evidence to targeted Tests Review in the same
Attempt, preserving completed history and leaving failed evidence uncredited.
Test: tests/binary.rs (interrupted_plan_host_evidence_recovers_same_attempt)
Test: tests/binary.rs (interrupted_plan_host_evidence_recovers_same_attempt_rejects_unsafe_state)
Test: tests/binary.rs (interrupted_plan_host_evidence_recovers_from_repaired_source)
Test: tests/binary.rs (interrupted_plan_host_evidence_rejects_changed_repair_provenance)
Unrelated interrupted Writers retain ordinary foreground and queued retry rules.
Test: tests/binary.rs (interrupted_owned_plan_writers_keep_ordinary_resume)

WHEN valid Plan host proof, a complete canonical run, a no-change Writer and
later targeted Review remain bound to the same candidate, THE SYSTEM SHALL
resume only outstanding requirements in the same Attempt. A later Review SHALL
NOT hide an unresolved canonical command or trigger a duplicate complete run.
Test: tests/binary.rs (combined_evidence_recovery_resumes_to_ready_candidate)
Test: tests/binary.rs (combined_evidence_recovery_resumes_to_ready_candidate_from_saved_history)

### B3

WHEN identical attachment or resume commands repeat while recovery remains
eligible, THE SYSTEM SHALL retain the recorded Task and dispatch identities,
rechecking candidate and evidence bindings through foreground and queued paths.
Test: tests/binary.rs (host_plan_recovery_public_commands_are_idempotent)
Test: tests/binary.rs (host_plan_recovery_public_commands_are_idempotent_recheck_changed_evidence)
This includes canonical-command and host recovery interrupted after immutable
evidence publication or targeted Review settlement. Repeated active queue resume
SHALL retain the dispatch; a later stop requires fresh explicit authorization.
Test: tests/binary.rs (evidence_recovery_retries_preserve_task_and_dispatch_identity)
Test: tests/binary.rs (canonical_attachment_resumes_settled_stop_and_rejects_live_owner)
Test: tests/binary.rs (evidence_recovery_receipt_cannot_clear_intervening_stop)

## Rebased landing verification

Exact Tester evidence means complete host-owned results bound to the candidate
commit under consideration. The applicable accepted baseline is trusted Tester
evidence for the exact target commit captured when landing starts.

### B1

WHEN capture-mode landing finishes rebasing a Merge Candidate and no reusable
exact Tester evidence names the post-rebase candidate commit,
THE SYSTEM SHALL run every configured Tester command against that candidate and
bind the resulting evidence to its exact commit before fast-forward.
Test: tests/binary.rs (work_merge_candidate_runs_tester_for_rebased_tip_before_land)

### B2

WHEN complete passing Tester evidence already names the exact post-rebase
candidate commit,
THE SYSTEM SHALL reuse that evidence without rerunning its Tester commands
during landing.
Test: tests/binary.rs (work_merge_candidate_reuses_exact_tester_evidence_before_land)

### B3

IF exact post-rebase Tester evidence contains a harness error or a test failure
beyond the applicable accepted baseline,
THEN THE SYSTEM SHALL stop landing before fast-forward and leave the target
branch unchanged.
Test: tests/binary.rs (work_merge_candidate_rebased_tester_failure_leaves_target_unchanged)
Test: tests/binary.rs (work_merge_candidate_rebased_tester_harness_error_leaves_target_unchanged)

### B4

WHEN exact post-rebase Tester verification blocks landing,
THE SYSTEM SHALL record merge status `failed`, an actionable failure reason,
and the exact Tester artifact on the Merge Candidate so the retained candidate
can be repaired and retried.
Test: tests/binary.rs (work_merge_candidate_rebased_tester_failure_records_retry_evidence)

### B5

WHEN exact post-rebase Tester evidence contains no harness error and no test
failure beyond the applicable accepted baseline,
THE SYSTEM SHALL continue through the remaining deterministic landing checks and
fast-forward boundary.
Test: tests/binary.rs (work_merge_candidate_rebased_tester_pass_continues_to_land)

## Corrective assignment context

### B1

WHEN Fluent captures a corrective Writer assignment that includes mutable
context, THE SYSTEM SHALL retain the assigned input independently of later
permitted updates to that context.
Test: tests/binary.rs (assignment_context_snapshot_survives_progress_updates)
Derived: Existing corrective assignments must survive ordinary Writer progress
updates without losing their original source obligations.

### B2

IF an assignment's immutable input or finding evidence changes, THEN THE SYSTEM
SHALL reject its use before advancement without accepting replacement bytes.
Test: tests/binary.rs (assignment_context_snapshot_rejects_changed_evidence)

### B3

WHEN an operator explicitly requests recovery of a settled candidate's saved
assignment that binds a recognized mutable-context path and supplies original
bytes matching that binding's recorded digest, THE SYSTEM SHALL retain an
audited immutable binding to those same bytes without changing current context
or launching work.
Test: tests/binary.rs (assignment_context_recovery_preserves_original_bytes)

### B4

IF that recovery lacks matching original bytes or has stale candidate or
assignment identity, active ownership, an unrelated input, or unsafe paths,
THEN THE SYSTEM SHALL reject it without changing the saved Work or protected
artifacts.
Test: tests/binary.rs (assignment_context_recovery_rejects_unsafe_inputs)

### B5

WHEN the same eligible recovery is repeated or resumed after interrupted
publication, THE SYSTEM SHALL reuse its recorded snapshot and operation
identity without duplicating work or clearing a later stop.
Test: tests/binary.rs (assignment_context_recovery_is_idempotent)

### B6

WHEN the operator separately resumes a successfully recovered Attempt, THE
SYSTEM SHALL continue only its outstanding native requirements while retaining
its completed Writer, source candidate, assignment findings, and prior evidence.
Test: tests/binary.rs (assignment_context_recovery_resumes_same_attempt)

### B7

WHEN status or show inspects a saved mutable-context binding, THE SYSTEM SHALL
report its state without publishing a snapshot, changing a binding, or granting
recovery authority.
Test: tests/binary.rs (assignment_context_recovery_inspection_is_read_only)

## Combined recovery state

### B1

WHEN a supported public mutation saves frozen assignment context and
carried-finding evidence together, THE SYSTEM SHALL preserve both authorities
and their original identities.
Test: tests/binary.rs (combined_recovery_preserves_context_and_carried_evidence)

### B2

WHEN an eligible settled Writer has accepted carried evidence and the operator
proves the exact original bytes of its changed legacy context, THE SYSTEM SHALL
recover that context without rewriting the carried binding or granting new
completion credit. Exact retries SHALL preserve the operation and later stops.
Test: tests/binary.rs (combined_recovery_repairs_context_with_carried_evidence)
Test: tests/binary.rs (combined_recovery_repairs_interrupted_context_with_carried_evidence)

### B3

IF context recovery receives changed proof or stale authority in combined state,
THEN THE SYSTEM SHALL reject it without changing protected history.
Test: tests/binary.rs (combined_recovery_rejects_changed_context_authority)

### B4

WHEN the combined coordinator mutates supported records or recovers pending
transactions from the preflight, assignment-context, or carried-evidence
predecessor, THE SYSTEM SHALL preserve the predecessor's recovery authority.
Test: tests/binary.rs (combined_recovery_preserves_source_version_records)

### B5

IF a predecessor cannot preserve combined recovery authority, THEN THE SYSTEM
SHALL fence its mutations of published records and pending transactions.
Test: tests/binary.rs (combined_recovery_older_readers_refuse_mutation)


## Output-free assignment context recovery

### B1

WHEN an operator supplies exact original-byte proof for a recognized mutable
context input assigned to a settled output-free Writer on the unchanged clean
candidate of its paused Attempt, THE SYSTEM SHALL publish the audited immutable
substitution without changing Task statuses, current context, or verification
credit.
Test: tests/binary.rs (assignment_context_recovery_output_free_preserves_state)
Modifies: Corrective assignment context:B3 — admits the bounded output-free
case as well as the existing completed-Writer case.

### B2

IF that request has changed proof, assignment, immutable evidence, candidate,
ownership, or unsupported later work, THEN THE SYSTEM SHALL reject it without
changing protected Work or artifacts.
Test: tests/binary.rs (assignment_context_recovery_output_free_rejects_drift)
Modifies: Corrective assignment context:B4 — preserves its checks for the
additional supported Task state.

### B3

WHEN the same eligible output-free context recovery is replayed, including
after partial publication, THE SYSTEM SHALL reuse its audit and snapshot while
preserving later stops.
Test: tests/binary.rs (assignment_context_recovery_output_free_replays_safely)
Modifies: Corrective assignment context:B5 — applies the same publication
guarantees to the additional supported Task state.

## Explicit retry after context repair

### B1

WHEN the operator separately authorizes completion recovery for the same
unchanged output-free Writer after its bound stale-context setup failure has
been repaired, THE SYSTEM SHALL make that Writer retryable with its original
obligations and retained failure history.
Test: tests/binary.rs (writer_completion_context_repaired_retry_keeps_assignment)
Example: A later unstarted duplicate may be superseded through an audit; it
cannot replace the failed Writer or consume another Writer round.
Derived: The actual paused Attempt otherwise remains unusable after its input
binding is repaired. This is not general permission to retry hard failures.

### B2

IF retry authorization is absent, stale, already consumed by a live or completed
launch, or invalidated by a later stop or candidate change, THEN THE SYSTEM SHALL
refuse another launch under that authorization.
Test: tests/binary.rs (writer_completion_context_repaired_retry_rejects_stale_authority)
Derived: Context substitution and retry remain separately authorized actions.

### B3

WHEN the authorized same-Task retry launches, THE SYSTEM SHALL retain the
production confinement boundary for preserved failure evidence, original inputs,
recovery audits, and unrelated operator state.
Test: tests/binary.rs (assignment_context_recovery_output_free_retry_real_confinement)
Derived: Retrying with retained failure history must not make that history
writable by the new worker.

## Output-free recovery guidance

### B1

WHEN status or show observes this paused shape, THE SYSTEM SHALL report the
current blocker and exactly eligible public recovery action without changing
saved authority.
Test: tests/binary.rs (assignment_context_recovery_output_free_reports_next_action)
Modifies: Corrective assignment context:B7 — identifies the new eligible
output-free case and its separate retry boundary.

## Combined coordinator recovery

### B1

WHEN an explicitly authorized same-Task retry completes after assignment-context
repair and current-candidate host proof passes targeted Tests Review,
THE SYSTEM SHALL deliver that accepted proof to ordinary Tests Review while
retaining canonical Tester and Learning requirements.
Test: tests/binary.rs (combined_context_retry_delivers_accepted_proof)

### B2

WHEN an operator authorizes an eligible failed-review round-cap Approach
revision with its separate bounded extension, THE SYSTEM SHALL preserve the
completed corrective Writer's assignment and immutable evidence while preparing
one new corrective Writer. Changed assignment evidence SHALL reject resume
without changing protected history.
Test: tests/binary.rs (combined_round_cap_revision_preserves_assignment_authority)

### B3

IF a predecessor cannot retain combined revision and assignment authority,
THEN THE SYSTEM SHALL prevent it from mutating published records or recovering
an interrupted revision, including through an otherwise-readable outer envelope.
The current coordinator SHALL retain supported predecessor records and recover
the complete current transaction without discarding earlier authority.
Test: tests/binary.rs (combined_installation_old_readers_preserve_authority)
## No-change Writer completion retry

### B1

WHEN an unchanged Writer leaves an invalid completion or no-change handoff, or
an earlier approved retry fails before provider launch, THE SYSTEM SHALL let an
operator approve one retry of the same Task through `repair-writer-completion`
and require a separate resume. A retained receipt from an earlier launch does
not block a retry when the current execution generation has no matching receipt.
Test: tests/binary.rs (no_change_completion_retry_resumes_initial_pause)
Test: tests/binary.rs (no_change_completion_retry_resumes_saved_prelaunch_failure)

### B2

WHEN the current clean candidate came from a later completed Writer than a
carried finding, THE SYSTEM SHALL bind that candidate to the host-owned Writer
baseline while preserving the finding's earlier assignment lineage.
Test: tests/binary.rs (no_change_completion_retry_keeps_carried_assignment)

### B3

WHEN Fluent prepares the retry, THE SYSTEM SHALL retain the original failure
files, create fresh outputs, and carry only authoritative earlier completion
claims; invalid result text supplies no passing credit.
Test: tests/binary.rs (no_change_completion_retry_rebuilds_without_credit)

### B4

IF the candidate, baseline, assignment, retained evidence, owner, cancellation,
or launch generation fails validation, THEN THE SYSTEM SHALL reject recovery
before changing protected Work or artifacts. A receipt for the current generation
or a malformed retained receipt blocks recovery.
Test: tests/binary.rs (no_change_completion_retry_rejects_invalid_authority)
Test: tests/binary.rs (no_change_completion_retry_rejects_current_or_invalid_receipt)

### B5

WHEN the operator repeats the same approval, including after interrupted
publication or a later stop, THE SYSTEM SHALL preserve its audit, operation,
Task, fresh output directory, and later state.
Test: tests/binary.rs (no_change_completion_retry_replays_publication)
Test: tests/binary.rs (no_change_completion_retry_preserves_later_stop)

### B6

WHEN foreground, queued, or direct Task execution reaches the retry, THE SYSTEM
SHALL apply the same admission and spawn checks and permit one provider launch.
Test: tests/binary.rs (no_change_completion_retry_queues_same_task)
Test: tests/binary.rs (no_change_completion_retry_rechecks_spawn)

### B7

WHEN status, show, or a resume refusal reports this failure, THE SYSTEM SHALL
name the eligible public action or the blocker without changing saved state.
Test: tests/binary.rs (no_change_completion_retry_guidance_is_read_only)

### B8

WHEN the retry launches, THE SYSTEM SHALL permit writes to its fresh outputs,
workspace, and private caches while protecting retained evidence and unrelated
operator files.
Test: tests/binary.rs (no_change_completion_retry_profile_rejects_changed_inputs)
Test: tests/binary.rs (no_change_completion_retry_real_confinement)

### B9

WHEN Fluent reads or publishes retry authority, THE SYSTEM SHALL retain older
supported history and prevent the pinned predecessor from mutating new records
or pending transactions, including after older envelopes are restored.
Test: tests/binary.rs (no_change_completion_retry_reads_supported_predecessor)
Test: tests/binary.rs (no_change_completion_retry_older_reader_refuses_mutation)

### B10

WHEN a completed no-change Writer has pending rows with partial claims, THE
SYSTEM SHALL allow an audited corrective Writer only after validating the
remaining work without crediting those claims, and SHALL retain the original
matrix bytes.
Test: src/writer_completion_recovery.rs (completed_no_change_with_pending_finding_schedules_corrective_writer)

### B11

WHEN a Writer Task completes, THE SYSTEM SHALL keep its Attempt active until
completion evidence, required host checks, reviews, Tester, and Learner gates
have been admitted.
Test: src/work_task_executor.rs (completed_writer_keeps_attempt_active_for_later_gates)

### B12

WHEN a retained historical Writer matrix cannot be parsed, THE SYSTEM SHALL
withhold its advisory completion identity without preventing validation of
the current Writer's matrix or changing the historical bytes.
Test: src/work_attempt_loop.rs (malformed_historical_matrix_cannot_block_current_writer_identity)

### B13

WHEN a passing Plan host Review belongs to an earlier Writer, THE SYSTEM SHALL
allow explicit historical settlement against its bound source evidence even
before the current Writer's matrix is ready, without granting current credit.
Test: src/host_evidence.rs (historical_plan_review_settles_before_current_matrix_is_ready)

## Reviewed canonical command replacements

### B1

WHEN targeted Tests Review accepts replacement host proof for a canonical
command, THE SYSTEM SHALL attribute its native failures only through a unique
configured harness or an exact synthetic command identity.
Test: src/verification_assessment.rs (reviewed_unique_harness_command_covers_its_native_failures)
Test: src/verification_assessment.rs (duplicate_harness_requires_exact_synthetic_command_identity)

### B2

WHEN an accepted command replacement covers a named test, THE SYSTEM SHALL
satisfy its reference in the derived assessment and retain the original
Tester result bytes.
Test: src/recovery_contracts.rs (reviewed_command_satisfies_named_reference_without_rewriting_results)

### B3

WHEN only `.fluent/expertise/` changes separate a reviewed Writer and its
descendant landing candidate, THE SYSTEM SHALL reuse accepted host proof only
for the same canonical command and its original attributable failures, after
revalidating the source artifacts, passing Review, and candidate ancestry.
New failures, ambiguous attribution, changed commands, or altered evidence
SHALL block reuse.
Test: src/tester_evidence.rs (accepted_command_covers_only_its_original_failed_tests)
Test: src/tester_evidence.rs (reviewed_host_proof_binds_source_and_expertise_ancestry)

## Landing with reviewed host proof

### B1

WHEN a post-rebase landing Tester has several failed canonical commands, each
with accepted host proof for its exact command occurrence and candidate, and
every remaining failure matches the accepted target baseline,
THE SYSTEM SHALL allow the landing comparison to continue if no native test
failure is new or lacks permitted attribution.
Test: src/tester_evidence.rs (reviewed_host_proof_covers_multiple_commands)
Test: src/work_merge_executor.rs (landing_accepts_reviewed_host_command_with_other_baseline_failures)

### B2

IF any failed canonical command lacks its own accepted proof or matching baseline
result, or any credited proof has a changed candidate, command occurrence,
manifest, applicability, source artifact, review state, or process settlement,
THEN THE SYSTEM SHALL reject the landing comparison before fast-forward and
retain the failed Merge Candidate for repair.
Test: src/tester_evidence.rs (reviewed_host_proof_rejects_missing_or_changed_command)
Test: src/tester_evidence.rs (reviewed_host_proof_binds_landing_comparison_inputs)

### B3

IF the landing Tester or an original Tester result bound to credited proof
contains a native failing test outside the accepted target baseline,
THEN THE SYSTEM SHALL reject landing unless that failure can be attributed
unambiguously to one reviewed command and each current failure was present in
that command's original Tester result.
Test: src/tester_evidence.rs (reviewed_host_proof_rejects_ambiguous_native_failures)
Test: src/work_merge_executor.rs (landing_rejects_original_native_failure_with_passing_command)

### B4

WHEN Fluent later validates retained evidence for a landed candidate that used
several reviewed host proofs,
THE SYSTEM SHALL apply the same command and native-failure rules against the
recorded landing Tester, target baseline, and manifest.
Test: src/work_merge_executor.rs (retained_history_checks_multiple_reviewed_host_proofs)
Test: src/work_merge_executor.rs (retained_history_checks_reviewed_host_proof_with_baseline_failures)

## Retry an interrupted landing

### B1

WHEN a landing retry finds an executing Rebase Task followed by a completed
Rebase Task, THE SYSTEM SHALL supersede the older reservation in the retry's
atomic state update and preserve the completed Task and result. A latest
executing Rebase Task SHALL block retry without changing saved state.
Test: src/work_merge_executor.rs (failed_landing_retry_restores_reviewed_attempt_atomically)

## SSH agent sources

### B1

WHEN the dashboard loads an explicitly configured SSH source,
THE SYSTEM SHALL show the remote project's published Work snapshot with its host
identity in the same project view as local sources.
Test: {"kind":"named-test","path":"tests/ssh_agent_sources.rs","name":"remote_snapshot_appears_with_source_identity"}

### B2

WHEN the operator attaches to a remote agent terminal,
THE SYSTEM SHALL attach to the verified invocation on that source in the
requested embedded or full-screen mode with the same Observe and Control rules.
Test: {"kind":"named-test","path":"tests/ssh_agent_sources.rs","name":"remote_attachment_preserves_invocation_and_control_mode"}
Test: {"kind":"whole-script","path":"tests/host/check-ssh-agent-terminal.sh"}

### B3

WHEN an SSH attachment disconnects,
THE SYSTEM SHALL label the terminal disconnected and discard unsent local input.
Test: {"kind":"named-test","path":"tests/ssh_agent_sources.rs","name":"ssh_disconnect_discards_unsent_input"}
Derived: Reconnection must not replay old keystrokes into a later interaction.

### B4

WHEN the operator reconnects to an SSH source,
THE SYSTEM SHALL resolve the same invocation again and show its current terminal
or an explicit unavailable result without restarting the agent.
Test: {"kind":"named-test","path":"tests/ssh_agent_sources.rs","name":"reconnect_never_restarts_or_retargets_worker"}

### B5

IF SSH authentication, host verification, or a remote prerequisite fails,
THEN THE SYSTEM SHALL report the source-specific remedy while keeping other
sources usable.
Test: {"kind":"named-test","path":"tests/ssh_agent_sources.rs","name":"ssh_prerequisite_failure_isolated_to_source"}

### B6

IF a remote response has an unsupported protocol version, malformed identity, or
unexpected endpoint,
THEN THE SYSTEM SHALL reject the response before using it to attach or control a
worker.
Test: {"kind":"named-test","path":"tests/ssh_agent_sources.rs","name":"invalid_remote_protocol_cannot_supply_attachment_authority"}

### B7

WHEN the dashboard opens an SSH source,
THE SYSTEM SHALL use the operator's explicitly selected SSH destination without
provisioning a host or copying provider credentials to it.
Test: {"kind":"named-test","path":"tests/ssh_agent_sources.rs","name":"ssh_source_uses_existing_host_without_provider_credentials"}
Derived: This feature accesses Fluent work already running on a configured host.

## Release acceptance

### B1

WHEN the operator verifies an explicitly identified Work Item, Attempt and exact
current clean commit for release, THE SYSTEM SHALL report success without
executing tests or changing Work state only when that commit has a ready or
landed Merge Candidate, verified successful Learning, passing effective ordinary
Reviews, and a complete settled canonical run whose configured command
occurrences are satisfied by passing results or candidate-bound host evidence
accepted by targeted Tests Review.
Test: tests/binary.rs (tester_verify_accepts_complete_candidate_bound_release)

### B2

IF the candidate, configuration, command occurrence, artifact digest, Review,
execution settlement or required result cannot be verified, THEN THE SYSTEM
SHALL reject release verification without changing Work state.
Test: tests/binary.rs (tester_verify_rejects_stale_or_incomplete_release_evidence)
Test: tests/binary.rs (tester_verify_rejects_advisory_baseline_substitution)

### B3

WHEN the release script publishes a release, THE SYSTEM SHALL first verify the
explicitly supplied Work Item and Attempt's acceptance for the exact release
HEAD while retaining every existing build, source, format, check, architecture,
signature and checksum gate.
Test: tests/behaviors/operations/test-release-script.sh

## OpenCode coder

THE SYSTEM SHALL recognize `opencode` as a valid Coder kind alongside
`claude`, `codex`, and `pi`. Specifying `opencode` via `--coder`,
`FLUENT_CODER`, or a coder mapping entry SHALL be accepted. Error messages,
help output, and any documentation of available Coders SHALL list `opencode`.
Test: src/coder.rs (coder_kind_resolves_opencode)
Test: src/coder.rs (coder_kind_serializes_opencode_as_kebab_case)
Test: src/coder.rs (coder_kind_rejects_unknown_lists_opencode)

## OpenCode invocation mechanics

### B1

WHEN Fluent invokes OpenCode for an autonomous role,
THE SYSTEM SHALL run `opencode run --standalone --auto --agent build` in the
role workspace, request JSONL output when a transcript path is configured, and
omit `--model` when no model is configured so OpenCode uses its own default.
Test: src/coder.rs (opencode_run_command_uses_v2_standalone_and_json)
Test: src/coder.rs (opencode_omits_json_format_without_transcript)
Test: src/coder.rs (opencode_command_omits_model_when_unset)
Test: src/coder.rs (opencode_sandbox_wraps_the_run_command)

### B2

WHEN Fluent resumes an OpenCode Writer session,
THE SYSTEM SHALL pass `--session <session-id>` and send a continuation message
that does not repeat the system prompt.
Test: src/coder.rs (opencode_resume_uses_session_and_continuation_prompt)

### B3

WHEN `FLUENT_MODEL` is set without a provider-qualified `provider/model` value,
THE SYSTEM SHALL NOT pass it to OpenCode, so OpenCode uses its own default
instead of failing on an unqualified name.
Test: src/coder.rs (opencode_model_requires_a_provider_qualified_global_model)

### B4

WHEN `FLUENT_MODEL` is set without a provider-qualified value and
`FLUENT_OPENCODE_MODEL` is unset,
THE SYSTEM SHALL warn once per process that the shared model was not applied.
Test: src/coder.rs (opencode_model_warns_only_for_an_unqualified_global_model)

## OpenCode readiness and failure handling

### B1

WHEN Fluent checks OpenCode readiness,
THE SYSTEM SHALL reject a version older than 2 with a typed readiness error that
names the upgrade command.
Test: src/provider_readiness.rs (opencode_version_major_parses_supported_release)
Test: src/provider_readiness.rs (opencode_rejects_v1_version)
Test: tests/binary.rs (init_with_opencode_default_rejects_opencode_v1)

### B2

WHEN Fluent classifies provider-unavailable evidence for an OpenCode transcript,
THE SYSTEM SHALL fail closed and report no unavailable evidence.
Test: src/provider_evidence.rs (opencode_has_no_prelude_or_rate_limit_evidence)

### B3

WHEN a caller selects OpenCode for Fargate execution,
THE SYSTEM SHALL reject the request with an explicit local-only error.
Test: src/fargate.rs (opencode_is_rejected_on_fargate)
Test: src/fargate.rs (mixed_mapping_rejects_a_local_only_review_coder)

## OpenCode isolation

### B1

WHEN Fluent prepares an autonomous OpenCode Writer,
THE SYSTEM SHALL create an Attempt-scoped OpenCode home, copy the host
`auth.json` and `opencode.json` or `opencode.jsonc` into it, and launch with
`HOME` set to that directory.
Test: src/work_task_executor.rs (opencode_home_seeds_auth_and_config)
Test: src/work_task_executor.rs (writer_launch_sets_opencode_home)

### B2

WHEN Fluent prepares an OpenCode home,
THE SYSTEM SHALL NOT copy operator plugins, agents, commands, or global
instructions.
Test: src/work_task_executor.rs (opencode_home_excludes_plugins_and_agents)

### B3

WHEN Fluent prepares an autonomous OpenCode reviewer or Learner,
THE SYSTEM SHALL use a home beneath that role's writable artifact or handoff
directory and set `HOME` to it.
Test: src/work_task_executor.rs (reviewer_launch_sets_opencode_home)
Test: src/work_task_executor.rs (learner_launch_sets_opencode_home)

### B4

WHEN Fluent renders the OpenCode Seatbelt profile,
THE SYSTEM SHALL include the OpenCode installer root for reading and SHALL NOT
grant the operator's `~/.config/opencode` or `~/.local/share/opencode` in
autonomous mode.
Test: src/os.rs (opencode_profile_grants_installer_root_without_operator_state)

### B5

WHEN Fluent starts an interactive OpenCode session,
THE SYSTEM SHALL append read/write grants for the operator's OpenCode config,
data, cache, and state directories.
Test: src/os.rs (grant_opencode_operator_state_appends_config_and_data_grants)
Test: tests/binary.rs (dry_run_with_opencode_uses_opencode_profile_layer_and_operator_grants)

### B6

WHEN an OpenCode trusted launch restricts its environment,
THE SYSTEM SHALL retain OpenCode's provider keys for that launch and SHALL NOT
widen the shared trusted allowlist for Claude, Codex, or Pi.
Test: src/coder.rs (opencode_trusted_launch_retains_only_its_provider_keys)

## OpenCode transcript identity

### B1

WHEN Fluent parses an OpenCode `step_start` event,
THE SYSTEM SHALL emit a session-init event carrying `sessionID` so Writer
continuity can find the session.
Test: src/transcript.rs (test_parse_opencode_step_start)
Test: src/work_attempt_loop.rs (writer_session_identity_accepts_opencode_transcript)

## OpenCode transcript projection

### B1

WHEN Fluent parses an OpenCode `text` event with non-empty text,
THE SYSTEM SHALL emit a text event for the activity feed.
Test: src/transcript.rs (test_parse_opencode_text)

### B2

WHEN Fluent parses an OpenCode completed `tool_use` event,
THE SYSTEM SHALL emit a tool-use event carrying the call ID, tool name, and a
human-readable summary derived from the tool title or its input path.
Test: src/transcript.rs (test_parse_opencode_tool_use)
Test: src/transcript.rs (test_parse_opencode_tool_use_uses_descriptive_title)
Test: src/transcript.rs (test_parse_opencode_todowrite_summary)

### B3

WHEN Fluent parses an OpenCode `step_finish` event whose reason is `stop` or is
absent,
THE SYSTEM SHALL emit a result event carrying the step cost.
Test: src/transcript.rs (test_parse_opencode_step_finish)
Test: src/transcript.rs (test_parse_opencode_step_finish_without_reason)

### B4

WHEN Fluent parses an OpenCode `step_finish` event whose reason is `tool-calls`,
THE SYSTEM SHALL NOT emit a session-complete result for that intermediate step.
Test: src/transcript.rs (test_parse_opencode_tool_calls_step_finish_is_silent)

### B5

WHEN Fluent parses an OpenCode `error` event,
THE SYSTEM SHALL project it as an unknown event type without failing the parse.
Test: src/transcript.rs (test_parse_opencode_error)

## OpenCode usage

### B1

WHEN Fluent parses an OpenCode transcript after Task completion,
THE SYSTEM SHALL emit one usage row per `step_finish` event, with input and
output tokens taken from `part.tokens`.
Test: src/usage.rs (extract_opencode_usage_returns_one_row_per_step_finish)

### B2

WHEN Fluent parses an OpenCode transcript after Task completion,
THE SYSTEM SHALL populate `cached_input_tokens` from `part.tokens.cache.read`
and `reasoning_output_tokens` from `part.tokens.reasoning`.
Test: src/usage.rs (extract_opencode_usage_reads_reasoning_and_cache_tokens)

### B3

WHEN an OpenCode `step_finish` follows a `step_start` event,
THE SYSTEM SHALL derive `duration_ms` from their timestamps.
Test: src/usage.rs (extract_opencode_usage_returns_one_row_per_step_finish)

### B4

IF an OpenCode transcript ends without a final `step_finish`,
THEN THE SYSTEM SHALL return the rows it observed without failing the Task.
Test: src/usage.rs (extract_opencode_usage_tolerates_missing_final_step_finish)

### B5

WHEN Fluent routes a completed OpenCode Task transcript to usage logging,
THE SYSTEM SHALL select the OpenCode extractor for the `opencode` coder string.
Test: src/usage.rs (extract_usage_for_coder_routes_opencode)

## OpenCode review evidence

### B1

WHEN a passing OpenCode Review Task writes inspection evidence,
THE SYSTEM SHALL accept the pass only when the transcript contains a completed
`tool_use` event whose input names the exact candidate and a declared inspected
file. A non-zero shell exit SHALL NOT count as inspection evidence.
Test: src/review_inspection.rs (accepts_successful_opencode_inspection)
Test: src/review_inspection.rs (rejects_failed_opencode_inspection)

## OpenCode init profile

### B1

WHEN `fluent init --coder-profile opencode-default --follow-up-mode <mode>` runs,
THE SYSTEM SHALL configure both Writer and reviewer roles with coder `opencode`
and an empty model so OpenCode's own default applies.
Test: src/setup.rs (configured_profile_opencode_default_sets_both_roles)

### B2

WHEN Fluent reports the available coder profiles,
THE SYSTEM SHALL include `opencode-default`.
Test: src/setup.rs (configured_profile_lists_opencode_default)

## OpenCode coder cross-cutting

### B1

WHEN this Work Item lands,
THE SYSTEM SHALL contain `Test:` references in `documentation/behaviors.md` for
each new EARS statement here, covering coder recognition, invocation, readiness,
isolation, transcript identity, review evidence, and the init profile.
Test: all EARS statements in the OpenCode coder sections above carry `Test:`
references
