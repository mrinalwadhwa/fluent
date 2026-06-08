# Behavior Tests

Tests verifying the behaviors defined in `documentation/behaviors.md`.

## Structure

```
tests/behaviors/
  skills/              ← scenario cards for skill behavior tests
    *.md               ← one scenario per test case
  operations/          ← scripts for operational behavior tests
    *.sh               ← one script per test case
```

## Skill behaviors

Tested via `tests/test-skill` — simulated two-agent conversations.
Each scenario exercises one or more behaviors from `documentation/behaviors.md`.
The harness disables tools, so these scenarios verify that a skill asks
for or discusses the right codebase context and produces the right
artifact shape; they do not verify actual file or code research.

Run a single scenario:
```sh
tests/test-skill tests/behaviors/skills/timeout-flag.md skills/capture-brief/SKILL.md
```

Run with automated judge:
```sh
tests/test-skill tests/behaviors/skills/timeout-flag.md skills/capture-brief/SKILL.md --judge
```

## Operational behaviors

Tested through the Rust binary integration tests, operation scripts, and
the remaining `tests/test-run` harness. These tests create temp projects,
run factory commands, and assert terminal output plus file system state.

```sh
cargo test --test binary
tests/test-run
for test in tests/behaviors/operations/*.sh; do bash "$test"; done
```

## Behavior mapping

### Brief capture

| Behavior | Scenarios |
|---|---|
| Interview user, research codebase, write brief.md | All capture-brief scenarios |
| Set status to `briefed`, write active-run | All capture-brief scenarios |
| Adapt depth — trivial request gets light pass | `fix-status-bug` |
| Adapt depth — vague idea gets sharpened | `parallel-runs`, `session-snapshots-not-useful` |
| Probe mechanics for partially clear requests | `code-reviewer`, `timeout-flag` |
| Ask for or discuss needed codebase context before follow-ups | All capture-brief scenarios |

### Behavior definition

| Behavior | Scenarios |
|---|---|
| Read brief + existing behaviors, write behaviors.diff.md | `run-summary-behaviors`, `format-check-behaviors` |
| Establish vocabulary before drafting behaviors | `run-summary-behaviors`, `format-check-behaviors` |
| Map actors, events, and states before EARS statements | `run-summary-behaviors`, `format-check-behaviors` |
| Produce observable EARS-style behavior increments | `run-summary-behaviors`, `format-check-behaviors` |
| Keep implementation choices out of behavior statements | `run-summary-behaviors`, `format-check-behaviors` |
| Set status to `behaviors-defined` | `run-summary-behaviors`, `format-check-behaviors` |

### Approach design

| Behavior | Scenarios |
|---|---|
| Research, evaluate options, write approach.md | `format-check-approach` |
| Load and cite relevant expertise references | `format-check-approach` |
| Present options with trade-offs and rejected alternatives | `format-check-approach` |
| Avoid manufacturing unnecessary design decisions | `format-check-approach` |
| Describe direction and boundaries without over-specifying internals | `format-check-approach` |
| Set status to `approach-designed` | `format-check-approach` |

### Execution planning

| Behavior | Scenarios |
|---|---|
| Break approach into steps, write plan.md | `format-check-plan` |
| Assess single run versus child-run decomposition | `format-check-plan` |
| Start with a walking skeleton | `format-check-plan` |
| Phrase steps as observable states with verification | `format-check-plan` |
| Map every behavior to a step or explicit TBD | `format-check-plan` |
| Identify scope trades and risks | `format-check-plan` |
| Set status to `planned` | `format-check-plan` |

### Operational (tested by test-run, binary.rs, and others)

| Behavior | Test |
|---|---|
| Version command reports package version and build metadata | `test-version.sh`, `binary.rs` |
| Work Item create writes a minimal item | `binary.rs`, `test-work-inspection.sh` |
| Work Item create rejects existing ids | `binary.rs`, `test-work-inspection.sh` |
| Work Item create rejects invalid ids | `binary.rs`, `test-work-inspection.sh` |
| Created Work Items appear through list and show | `binary.rs`, `test-work-inspection.sh` |
| Work Item list prints stored ids and titles | `binary.rs`, `test-work-inspection.sh` |
| Work Item list prints an empty state with no items | `binary.rs`, `test-work-inspection.sh` |
| Work Item show prints pretty JSON | `binary.rs`, `test-work-inspection.sh` |
| Work Item show reports missing items | `binary.rs`, `test-work-inspection.sh` |
| Work Item inspection reports invalid stored state | `binary.rs`, `test-work-inspection.sh` |
| Work Item intake and inspection are independent from legacy runs | `binary.rs`, `test-work-inspection.sh` |
| Work Item attempt command adds planned Attempt and initial write Task | `work_model_external.rs`, `binary.rs`, `test-work-attempt-intake-review.sh` |
| Work Item attempt command rejects missing, duplicate, and invalid ids without changing state | `work_model_external.rs`, `binary.rs`, `test-work-attempt-intake-review.sh` |
| Work Task run creates or reuses the writable worktree and launches the coder there | `binary.rs`, `test-work-task-run.sh` |
| Work Task run completes only with clean committed output from the current run | `binary.rs`, `test-work-task-run.sh` |
| Work Task run rejects dirty, no-output, coder-failure, and invalid Task requests without completing | `binary.rs`, `test-work-task-run.sh` |
| Work review plans read-only review Tasks with artifact areas | `binary.rs`, `test-work-task-run.sh` |
| Work Task run completes review Tasks from durable artifacts while preserving verdict boundaries | `binary.rs`, `test-work-task-run.sh` |
| Work Attempt run advances planned Tasks, plans reviews, and interprets review outcomes | `binary.rs`, `test-work-attempt-loop.sh` |
| Work Attempt run creates follow-up writes or needs-user handoffs at review boundaries | `binary.rs`, `test-work-attempt-loop.sh` |
| Merge Candidate inspection prints stored candidate JSON without changing state | `binary.rs`, `test-work-merge-candidate.sh` |
| Work merge executes ready Merge Candidates only after validating candidate ownership, provenance, source branch, target safety, checks, and reviewers | `binary.rs`, `test-work-merge-candidate.sh` |
| Work merge rejects invalid stored candidate provenance without rewriting state | `binary.rs`, `test-work-merge-candidate.sh` |
| Work merge records durable failed state for rebase, check, review, and late target-move failures | `binary.rs`, `test-work-merge-candidate.sh` |
| Work merge records landed commit and artifacts after fast-forwarding the target branch | `binary.rs`, `test-work-merge-candidate.sh` |
| Create worktree from current HEAD | `test-run` |
| Branch from non-main branch | `test-run` |
| Run-id resolution priority chain | `test-run` |
| Worktree copies all run state files | `binary.rs` |
| Worktree records source-branch and worktree path | `binary.rs` |
| Run-id scan ignores completed runs | `binary.rs` |
| Status display includes runtime and brief | `binary.rs` |
| Worktree copies scope file | `binary.rs` |
| Run-id scan treats `executing` as active | `binary.rs` |
| Run-id scan skips `needs-user` and `failed` | `binary.rs` |
| Status display works with no runs | `test-status-edges`, `binary.rs` |
| Review mode copies mode/reviewers to worktree | `src/worktree.rs` |
| Resume finds `needs-user` or `failed` runs | `test-resume-resolve`, `binary.rs` |
| Headless resume restarts a selected run | `test-headless-resume`, `binary.rs` |
| Headless resume rejects parallel parent runs | `test-headless-resume`, `binary.rs` |
| Status displays fargate runtime | `test-watch-and-status-edges` |
| Status displays mixed runtimes | `test-watch-and-status-edges` |
| Watch polls at default interval | `test-watch-and-status-edges` |
| Watch accepts custom interval | `test-watch-and-status-edges` |
| Watch displays run status | `test-watch-and-status-edges` |
| Notification includes run ID, status, and brief | `test-notification-content` |
| Complete notification includes session count and review verdict | `test-notification-content` |
| Needs-user notification includes handoff content | `test-notification-content` |

### Session loop (tested by binary.rs and src/session.rs)

| Behavior | Test |
|---|---|
| Session loop restarts on `executing` | `binary.rs`, `src/session.rs` |
| Session loop stops on terminal status | `binary.rs`, `src/session.rs` |
| Consecutive failure guard (3 strikes) | `binary.rs` |
| Max session limit (50) | `binary.rs` |
| Session loop uses handoff prompt | `binary.rs` |
