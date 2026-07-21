# Testing in the fluent

The fluent has several types of tests:

**Behavioral tests** (`tests/behaviors/`) — verify the fluent delivers its specified behaviors. Written from EARS statements without seeing code. Test the system from the outside.

**Skill tests** (`tests/test-skill`) — simulate skill conversations between two agents. Test skill structure and flow.

Each type catches a different class of problems. Behavioral tests catch user-visible regressions. Skill tests catch conversation design issues.

## Where failure logs live

Every test run writes per-case stdout and stderr to `tests/output/`:

- Rust binary tests: `tests/output/<test-name>.log`
- Shell behavior tests: `tests/output/<test-file-name>/<case>.log`

The `tests/output/` directory is gitignored — it exists only on disk. After a failure, open the log directly instead of rerunning.

Set `FLUENT_TESTS_SKIP_LOG=1` to disable per-case log writing (useful for CI workers that manage their own artifact capture).

## Patterns

- [flock-lease-tests-under-libtest](testing/patterns/flock-lease-tests-under-libtest.md) — read when writing unit tests for `lease`-based singletons that must also pass under `cargo test` (libtest threads), not just nextest.
- [unique-ids-for-attempt-worktrees](testing/patterns/unique-ids-for-attempt-worktrees.md) — read when a binary test drives a real Attempt or `fluent scheduler run`, so its sibling worktrees in the shared temp root do not collide across runs.
- [inject-stage-failure-via-filesystem-obstruction](testing/patterns/inject-stage-failure-via-filesystem-obstruction.md) — read when a test must fail one stage of a journaled pipeline (Observation/Work/queue) while earlier stages and the outer land succeed.
- [observe-sandboxed-learner-via-transcript](testing/patterns/observe-sandboxed-learner-via-transcript.md) — read when a binary test drives a sandboxed handoff-only Learner and needs to observe its run count, prompt, or commit, which it cannot record in shared-temp files.
