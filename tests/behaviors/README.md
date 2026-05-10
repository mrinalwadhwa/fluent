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
| Read brief + existing behaviors, write behaviors.diff.md | (needs define-behaviors scenarios) |
| Set status to `behaviors-defined` | (needs define-behaviors scenarios) |

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

### Operational (tested by test-run and test-run-state)

| Behavior | Test |
|---|---|
| Create worktree from current HEAD | `test-run` |
| Branch from non-main branch | `test-run` |
| Run-id resolution priority chain | `test-run` |
| Worktree copies all run state files | `test-run-state` |
| Worktree records source-branch and worktree path | `test-run-state` |
| Run-id scan ignores completed runs | `test-run-state` |
| Status display includes backend and brief | `test-run-state` |

### Operational (not yet tested)

| Behavior | Reason |
|---|---|
| Session loop restarts on `executing` | Requires Claude API |
| Session loop stops on terminal status | Requires Claude API |
| Consecutive failure guard (3 strikes) | Requires Claude API |
| Max session limit (50) | Requires Claude API |
