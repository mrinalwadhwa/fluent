# Behaviors

Observable behaviors of the factory system. Each statement describes what
the system does, not how. EARS format.

## Test harnesses

| Harness | Runs | Usage |
|---|---|---|
| `tests/test-skill` | Skill conversation simulations | `tests/test-skill <scenario> <skill> [--judge]` |
| `tests/test-run` | Operational assertions | `tests/test-run` |

---

## Brief capture

WHEN the user invokes the capture-brief skill,
THE SYSTEM SHALL interview the user, research the codebase, and write
a brief.md to `.factory/runs/[run-id]/`.
Test: tests/behaviors/skills/code-reviewer.md (test-skill)

WHEN the brief is confirmed by the user,
THE SYSTEM SHALL set status to `briefed` and write `.factory/active-run`
with the run-id.

## Behavior definition

WHEN the user invokes the define-behaviors skill,
THE SYSTEM SHALL read the brief and existing behaviors, elaborate into
EARS-format behavioral statements, and write behaviors.diff.md.

WHEN behaviors are approved by the user,
THE SYSTEM SHALL set status to `behaviors-defined`.

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
Test: src/worktree.rs (setup_run_worktree tests), tests/binary.rs (worktree creates and copies state), tests/behaviors/operations/test-run-state.sh

WHEN `factory run` is invoked from a non-main branch,
THE SYSTEM SHALL branch the worktree from that branch and record it as
the source-branch.
Test: tests/test-run (setup_run_worktree from non-main branch)

## Session loop (local)

WHEN `factory run` is invoked with the local runtime,
THE SYSTEM SHALL launch the selected coder in non-interactive mode with
the brief or handoff as the initial prompt.
Test: tests/behaviors/operations/test-session-loop.sh (initial prompt uses brief, initial prompt uses handoff)

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
Test: tests/behaviors/operations/test-session-loop.sh (loop restarts on executing)

WHEN the agent exits with status `needs-user`, `complete`, or `failed`,
THE SYSTEM SHALL stop the loop.
Test: tests/behaviors/operations/test-session-loop.sh (loop stops on needs-user, loop stops on failed), tests/behaviors/operations/test-review-phase.sh (complete with passing reviews stops loop)

WHEN the agent exits with status `rate-limited`,
THE SYSTEM SHALL wait 5 minutes and restart the agent.
Test: tests/behaviors/operations/test-session-loop.sh (loop restarts on rate-limited)

IF the agent exits with a non-zero exit code 3 consecutive times,
THEN THE SYSTEM SHALL set status to `failed` and stop the loop.
Test: tests/behaviors/operations/test-session-loop.sh (consecutive failures set failed, success resets failure counter)

IF the session count exceeds 50,
THEN THE SYSTEM SHALL set status to `failed` and stop the loop.
Test: tests/behaviors/operations/test-session-loop.sh (max sessions sets failed)

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
before running new reviews. Review files are copied; transcript files
are moved.
Test: src/review.rs (test_archive_previous_round_copies_reviews, test_archive_previous_round_noop_for_first_round), tests/binary.rs (run_archives_review_rounds), tests/behaviors/operations/test-observability.sh

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
THE SYSTEM SHALL upload the worktree to S3 and start an ECS Fargate task.

WHEN `factory run --runtime fargate --coder codex` is invoked,
THE SYSTEM SHALL fail with a clear unsupported-coder error.
Test: tests/binary.rs (run_fargate_with_codex_fails_before_config)

WHEN the Fargate task starts,
THE SYSTEM SHALL pull the workspace from S3 and run the session loop.

WHEN the Fargate task reaches a terminal status,
THE SYSTEM SHALL upload the workspace to S3.

## Status reporting

WHEN `factory status` is invoked,
THE SYSTEM SHALL display all runs with their status, runtime, and brief
summary.
Test: tests/test-run (test_status_display), tests/behaviors/operations/test-run-state.sh

WHEN `factory status` is invoked and a Fargate run exists,
THE SYSTEM SHALL check S3 for a completed workspace and query the ECS API
for task state.

## Workspace retrieval

WHEN `factory pull` is invoked,
THE SYSTEM SHALL download the completed workspace from S3 into the run's
worktree directory.

## Interactive access

WHEN `factory shell` is invoked,
THE SYSTEM SHALL open an interactive shell into the running Fargate
container via ECS Exec.

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
Test: tests/behaviors/operations/test-review-phase.sh (complete with passing reviews stops loop, review failure restarts author), tests/behaviors/operations/test-reviewing-status.sh (status is reviewing while reviewers run, status transitions to complete when all pass, status is executing before author restarts on failure)

WHEN all reviewers return verdict `pass`,
THE SYSTEM SHALL accept the run as complete and stop the loop when the
run worktree has no tracked changes, staged changes, or untracked
non-ignored files outside `.factory`.
Test: tests/behaviors/operations/test-review-phase.sh (all reviewers pass returns zero, complete with passing reviews stops loop)

WHEN any reviewer returns verdict `fail` or `uncertain`,
THE SYSTEM SHALL set status back to `executing` and restart the author
with the review findings.
Test: tests/behaviors/operations/test-review-phase.sh (reviewer fail returns non-zero, reviewer uncertain returns non-zero, review failure restarts author)

## Review runs

WHEN `factory run` is invoked and the run's mode is `review`,
THE SYSTEM SHALL set status to `reviewing`, run reviewers with
full-codebase scope, and produce findings. No author session is
launched; the run completes after one review round.
Test: tests/behaviors/operations/test-review-phase.sh (review run all pass completes without author)

WHEN `factory run` is invoked and the run has a `scope` file,
THE SYSTEM SHALL copy the scope file into the worktree.
Test: src/worktree.rs (test_worktree_copies_scope_file)

WHEN a review run completes its single review round,
THE SYSTEM SHALL set status to `complete` and stop without launching
the author, regardless of reviewer verdict.
Test: tests/behaviors/operations/test-review-phase.sh (review run all pass completes without author)

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
Test: tests/behaviors/operations/test-review-round-limit.sh (review round limit completes after 10 cycles)

## Parent death detection

WHILE `factory watch` is running,
IF the parent process exits (ppid changes),
THEN THE SYSTEM SHALL stop polling and exit.
Test: tests/behaviors/operations/test-watch-timeout.sh (watch detects parent exit)

## Resume

WHEN `factory resume` is invoked,
THE SYSTEM SHALL find a run with status `needs-user` or `failed` and
launch an interactive agent session for that run.
Test: tests/behaviors/operations/test-resume-resolve.sh

## Land

WHEN `factory land` is invoked and the run status is not `complete`,
THE SYSTEM SHALL refuse and exit non-zero.
Test: tests/behaviors/operations/test-land.sh (land rejects non-complete run), tests/binary.rs (land_rejects_non_complete_run)

WHEN `factory land` is invoked and any review has verdict `fail`,
`uncertain`, or is missing a verdict line,
THE SYSTEM SHALL refuse and exit non-zero.
Test: tests/behaviors/operations/test-land.sh (land rejects fail review verdict, land rejects uncertain review verdict), tests/binary.rs (land_rejects_failed_reviews)

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

WHILE a run is actively executing (author or reviewers running),
THE SYSTEM SHALL show a visual indicator that distinguishes "active"
from "idle" at a glance.
Test: dashboard::tests::test_header_spinner_advances_with_tick, dashboard::tests::test_agent_tab_running_shows_spinner_symbol, dashboard::tests::test_header_author_executing_shows_spinner, tests/behaviors/operations/test-dashboard-activity.sh (no crash when run is actively executing, no crash when reviewers are running)

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
rate-limited, planned). The `reviewing` phase shows a spinner.
Test: dashboard::tests::test_header_reviewing_shows_progress, dashboard::tests::test_header_complete_no_spinner, dashboard::tests::test_header_failed_no_spinner, dashboard::tests::test_compute_phase_needs_user, dashboard::tests::test_compute_phase_rate_limited, dashboard::tests::test_header_rate_limited_shows_spinner, dashboard::tests::test_compute_phase_planned, tests/behaviors/operations/test-dashboard-activity.sh (no crash when run has failed, no crash when run needs user input, no crash with mixed run states)

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
display that tab's transcript or report content in the activity feed,
reset the scroll position, and re-enable auto-scroll.

WHEN the user presses Shift-Tab,
THE SYSTEM SHALL select the previous agent tab within the current run,
display that tab's transcript or report content in the activity feed,
reset the scroll position, and re-enable auto-scroll.

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
THE SYSTEM SHALL render frames at ~100ms intervals for smooth animation
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

WHEN `factory run --coder codex` is invoked with the sandboxed local runtime,
THE SYSTEM SHALL launch Codex under `sandbox-exec` with Factory's
Seatbelt profile, approval policy `never`, and the run worktree as
`--cd`, while disabling Codex's own sandbox. The rendered profile SHALL
include `common.sb` plus the Codex-specific `codex.sb` layer. The
Codex process SHALL receive `SSL_CERT_FILE` for a file-based CA bundle
when the caller has not already set it.
Test: tests/behaviors/operations/test-codex-runtime.sh (sandboxed codex uses factory seatbelt), tests/behaviors/operations/test-codex-approval-flag.sh (approval-policy flag appears before exec)

WHEN `factory run --coder codex --no-sandbox` is invoked,
THE SYSTEM SHALL launch Codex with
`--dangerously-bypass-approvals-and-sandbox`.
Test: tests/binary.rs (run_with_codex_uses_exec_json_and_status_contract)

WHILE running inside the sandbox,
THE SYSTEM SHALL inject credentials via environment variables, never by
opening filesystem access to credential stores.
Test: tests/behaviors/operations/test-sandbox.sh (profile denies Keychain Mach services, profile denies credential filesystem access, credentials injected via env vars)
