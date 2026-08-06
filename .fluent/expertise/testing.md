# Testing in the fluent

Fluent has four complementary test surfaces:

**Rust unit tests** (`src/`) — exercise module-local logic and invariants.

**Rust integration tests** (`tests/*.rs`) — exercise the public API and real CLI routes. The configured suite runs them with `cargo nextest` and the `test-support` feature.

**Behavior scripts** (`tests/behaviors/operations/`) — test user-visible EARS behaviors against the built binary from the outside.

**Skill scenarios** (`tests/behaviors/skills/`, driven by `tests/test-skill` and companion shell scripts) — simulate conversations and verify interactive skill structure and flow.

`.fluent/tester.yaml` is the canonical full-suite definition. Writers should run the smallest harness-native selection that covers their change and report it as advisory focused verification. For code-producing Work, Fluent runs the complete configured suite once, after code review, as the authoritative final Tester gate. Review-only Work retains Tester-first ordering.

## Where failure logs live

Every test run writes per-case stdout and stderr to `tests/output/`:

- Rust binary tests: `tests/output/<test-name>.log`
- Shell behavior tests: `tests/output/<test-file-name>/<case>.log`

The `tests/output/` directory is gitignored — it exists only on disk. After a failure, open the log directly instead of rerunning.

Set `FLUENT_TESTS_SKIP_LOG=1` to disable per-case log writing (useful for CI workers that manage their own artifact capture).

## Distinguish synthetic and real Seatbelt preflights

The configured test-support suite sets `FLUENT_TEST_HOST_SANDBOX_PREFLIGHT=pass`
so route tests can cross the host gate hermetically. That makes
`os::preflight_profile` a synthetic success; it does not prove that the current
host can apply a nested Seatbelt profile.

A macOS test that executes a real `/usr/bin/sandbox-exec` confinement assertion
must first probe the rendered profile through `os::preflight_profile_with` and a
real harmless command, bypassing the test-support override. Skip the real
confinement assertion only when that probe reports the parent-sandbox
`Operation not permitted` condition. Keep route tests on the synthetic seam and
real profile-enforcement tests on the explicit host probe.

## Patterns

- [flock-lease-tests-under-libtest](testing/patterns/flock-lease-tests-under-libtest.md) — read when writing unit tests for `lease`-based singletons that must also pass under `cargo test` (libtest threads), not just nextest.
- [unique-ids-for-attempt-worktrees](testing/patterns/unique-ids-for-attempt-worktrees.md) — read when a binary test drives a real Attempt or `fluent scheduler run`, so its sibling worktrees in the shared temp root do not collide across runs.
- [inject-stage-failure-via-filesystem-obstruction](testing/patterns/inject-stage-failure-via-filesystem-obstruction.md) — read when a test must fail one stage of a journaled pipeline (Observation/Work/queue) while earlier stages and the outer land succeed.
- [observe-sandboxed-learner-via-transcript](testing/patterns/observe-sandboxed-learner-via-transcript.md) — read when a binary test drives a sandboxed handoff-only Learner and needs to observe its run count, prompt, or commit, which it cannot record in shared-temp files.
- [reproduce-console-pipe-pressure](testing/patterns/reproduce-console-pipe-pressure.md) — read when a binary test must prove an unread or saturated writer console cannot stall transcript capture.
- [stateful-fake-cli-for-orchestration-tests](testing/patterns/stateful-fake-cli-for-orchestration-tests.md) — read when testing an operator shell script that sequences `fluent` commands and must be verified without a model, network, or the real home.
