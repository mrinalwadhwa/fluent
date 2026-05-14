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
approach.md with key technical decisions and solution direction.

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
THE SYSTEM SHALL launch Claude in print mode with the brief or handoff
as the initial prompt.
Test: tests/behaviors/operations/test-session-loop.sh (initial prompt uses brief, initial prompt uses handoff)

WHEN the agent exits with status `executing`,
THE SYSTEM SHALL capture a session snapshot and restart the agent.
Test: tests/behaviors/operations/test-session-loop.sh (loop restarts on executing)

WHEN the agent exits with status `needs-user`, `complete`, or `failed`,
THE SYSTEM SHALL capture a session snapshot and stop the loop.
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
Test: src/session.rs (test_loop_writes_sessions_log, test_loop_writes_nonzero_exit_to_sessions_log), tests/behaviors/operations/test-observability.sh

WHEN the session loop launches an agent session,
THE SYSTEM SHALL pass `--verbose --output-format stream-json` and pipe
stdout to `sessions/session-N/transcript.jsonl`.
Test: src/session.rs (test_loop_creates_session_transcript_dir), tests/behaviors/operations/test-observability.sh

## Review archiving

WHEN a review round fails and a new round starts,
THE SYSTEM SHALL archive previous review artifacts to `reviews/round-N/`
before running new reviews. Review files are copied; transcript files
are moved.
Test: src/review.rs (test_archive_previous_round_copies_reviews, test_archive_previous_round_noop_for_first_round), tests/behaviors/operations/test-observability.sh

WHEN a reviewer runs,
THE SYSTEM SHALL capture its stream-json output to
`reviews/transcript-{name}.jsonl`.
Test: tests/behaviors/operations/test-observability.sh

## Session loop (local) — credential refresh

WHEN a new session starts on the local runtime,
THE SYSTEM SHALL run an unsandboxed Claude invocation to refresh the
OAuth token, then re-inject credentials from Keychain.

## Fargate execution

WHEN `factory run --runtime fargate` is invoked,
THE SYSTEM SHALL upload the worktree to S3 and start an ECS Fargate task.

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
THE SYSTEM SHALL fire a macOS notification.

## Run-id resolution

WHEN a factory command needs the run-id,
THE SYSTEM SHALL check in order: `--run-id` flag, `FACTORY_RUN_ID` env
var, `.factory/active-run` file, then scan for active runs. The scan
considers a run active if its status is `planned` or `executing`.
Test: src/run.rs (resolve run-id tests), tests/binary.rs (run-id resolution tests)

## Review phase

WHEN the author sets status to `complete`,
THE SYSTEM SHALL run all reviewers in parallel before accepting completion.
Test: tests/behaviors/operations/test-review-phase.sh (complete with passing reviews stops loop, review failure restarts author)

WHEN all reviewers return verdict `pass`,
THE SYSTEM SHALL accept the run as complete and stop the loop.
Test: tests/behaviors/operations/test-review-phase.sh (all reviewers pass returns zero, complete with passing reviews stops loop)

WHEN any reviewer returns verdict `fail` or `uncertain`,
THE SYSTEM SHALL set status back to `executing` and restart the author
with the review findings.
Test: tests/behaviors/operations/test-review-phase.sh (reviewer fail returns non-zero, reviewer uncertain returns non-zero, review failure restarts author)

## Review runs

WHEN `factory run` is invoked and the run's mode is `review`,
THE SYSTEM SHALL run reviewers first (before the author) with full-codebase
scope, then pass findings to the author.
Test: tests/behaviors/operations/test-review-phase.sh (review run findings launch author)

WHEN `factory run` is invoked and the run has a `scope` file,
THE SYSTEM SHALL copy the scope file into the worktree.
Test: src/worktree.rs (test_worktree_copies_scope_file)

WHEN reviewers all pass on a review run's initial review,
THE SYSTEM SHALL set status to `complete` and stop the loop without
launching the author.
Test: tests/behaviors/operations/test-review-phase.sh (review run all pass completes without author)

## Resume

WHEN `factory resume` is invoked,
THE SYSTEM SHALL find a run with status `needs-user` or `failed` and
launch an interactive agent session for that run.
Test: tests/behaviors/operations/test-resume-resolve.sh

## Sandbox (local)

WHILE running on the local runtime,
THE SYSTEM SHALL execute the agent inside a macOS Seatbelt sandbox with
filesystem access restricted to the workspace root.

WHILE running inside the sandbox,
THE SYSTEM SHALL inject credentials via environment variables, never by
opening filesystem access to credential stores.
