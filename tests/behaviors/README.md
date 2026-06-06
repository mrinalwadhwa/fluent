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

Run a single scenario:
```sh
tests/test-skill tests/behaviors/skills/timeout-flag.md skills/capture-brief/SKILL.md
```

Run with automated judge:
```sh
tests/test-skill tests/behaviors/skills/timeout-flag.md skills/capture-brief/SKILL.md --judge
```

## Operational behaviors

Tested via `tests/test-run` — creates a temp project, runs
factory commands, asserts file system state.

```sh
tests/test-run
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
| Research codebase before follow-ups | All capture-brief scenarios |

### Behavior definition

| Behavior | Scenarios |
|---|---|
| Read brief + existing behaviors, write behaviors.diff.md | `run-summary-behaviors` |
| Set status to `behaviors-defined` | `run-summary-behaviors` |

### Approach design

| Behavior | Scenarios |
|---|---|
| Research, evaluate options, write approach.md | (needs design-approach scenarios) |
| Set status to `approach-designed` | (needs design-approach scenarios) |

### Execution planning

| Behavior | Scenarios |
|---|---|
| Break approach into steps, write plan.md | (needs plan-execution scenarios) |
| Set status to `planned` | (needs plan-execution scenarios) |

### Operational (tested by test-run, test-run-state, and others)

| Behavior | Test |
|---|---|
| Version command reports package version and build metadata | `test-version.sh`, `binary.rs` |
| Create worktree from current HEAD | `test-run` |
| Branch from non-main branch | `test-run` |
| Run-id resolution priority chain | `test-run` |
| Worktree copies all run state files | `test-run-state` |
| Worktree records source-branch and worktree path | `test-run-state` |
| Run-id scan ignores completed runs | `test-run-state` |
| Status display includes runtime and brief | `test-run-state` |
| Worktree copies scope file | `test-run-state`, `binary.rs` |
| Run-id scan treats `executing` as active | `test-run-state`, `binary.rs` |
| Run-id scan skips `needs-user` and `failed` | `test-run-state`, `binary.rs` |
| Status display works with no runs | `test-status-edges`, `binary.rs` |
| Review mode copies mode/reviewers to worktree | `test-review-phase` |
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

### Session loop (tested by binary.rs and test-session-loop.sh)

| Behavior | Test |
|---|---|
| Session loop restarts on `executing` | `binary.rs`, `test-session-loop` |
| Session loop stops on terminal status | `binary.rs`, `test-session-loop` |
| Consecutive failure guard (3 strikes) | `binary.rs` |
| Max session limit (50) | `binary.rs` |
| Session loop uses handoff prompt | `binary.rs` |
