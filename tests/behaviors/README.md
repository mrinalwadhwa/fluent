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

Tested through the Rust binary integration tests and operation scripts.
These tests create temp projects, run fluent commands, and assert
terminal output plus file system state.

```sh
cargo test --test binary
for test in tests/behaviors/operations/*.sh; do bash "$test"; done
```

Merge reviewers who need to keep a candidate workspace read-only can
build Fluent under their artifact directory and pass that binary to
behavior operation scripts:

```sh
CARGO_TARGET_DIR="$REVIEW_ARTIFACT_DIR/target" cargo build
FLUENT_BIN_OVERRIDE="$REVIEW_ARTIFACT_DIR/target/debug/fluent" \
  bash tests/behaviors/operations/test-work-task-run.sh
```

## Behavior mapping

### Brief capture

| Behavior | Scenarios |
|---|---|
| Interview user, research codebase, write brief.md | All capture-brief scenarios |
| Produce approved brief for Work Item planning context | All capture-brief scenarios |
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
| Assess one Work Item, likely follow-up Task notes, or peer Work Items | `format-check-plan`, `parallel-work-items-plan` |
| Start with a walking skeleton | `format-check-plan` |
| Phrase steps as observable states with verification | `format-check-plan` |
| Map every behavior to a step or explicit TBD | `format-check-plan` |
| Identify scope trades and risks | `format-check-plan` |
| Create Work Item planning context or use legacy planned fallback | `format-check-plan` |
| Prefer peer Work Items for independent parallel work | `parallel-work-items-plan` |
| Define sync points without default Task dependencies or child-run groups | `parallel-work-items-plan` |

### Operational (tested by binary.rs and operation scripts)

| Behavior | Test |
|---|---|
| Behavior operation scripts accept `FLUENT_BIN_OVERRIDE` and default to `target/debug/fluent` | `test-behavior-bin-override.sh` |
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
| Work merge records merged commit and artifacts after fast-forwarding the target branch | `binary.rs`, `test-work-merge-candidate.sh` |
