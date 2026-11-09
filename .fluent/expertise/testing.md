# Testing in the fluent

## Patterns

- [build-merge-ready-fixtures](testing/patterns/build-merge-ready-fixtures.md) — read when testing local Candidate actions through persisted dashboard state.

- [native-websocket-pressure](testing/patterns/native-websocket-pressure.md) — read when testing concurrent input and partial native display writes

- [observe-descendant-socket-eof](testing/patterns/observe-descendant-socket-eof.md) — read when proving that process cleanup closes a descendant's inherited descriptor

- [verified-historical-readers](testing/patterns/verified-historical-readers.md) — read when adding pinned historical executable fixtures that run across worktrees.

Fluent has four complementary test surfaces:

**Rust unit tests** (`src/`) — exercise module-local logic and invariants.

**Rust integration tests** (`tests/*.rs`) — exercise the public API and real CLI routes. The configured suite runs them with `cargo nextest` and the `test-support` feature.

**Behavior scripts** (`tests/behaviors/operations/`) — test user-visible EARS behaviors against the built binary from the outside.

**Skill scenarios** (`tests/behaviors/skills/`, driven by `tests/test-skill` and companion shell scripts) — simulate conversations and verify interactive skill structure and flow.

`.fluent/tester.yaml` is the canonical full-suite definition. Writers should run the smallest harness-native selection that covers their change and report it as advisory focused verification. For code-producing Work, Fluent runs the complete configured suite once, after code review, as the authoritative final Tester gate. Review-only Work retains Tester-first ordering.

A Work Task Tester may execute tools that rewrite tracked lockfiles or generated
metadata. The host admits only a clean tracked candidate at the expected commit,
captures the Tester evidence, restores tracked index/worktree changes after each
launch, and records the mutation. It does not delete untracked or ignored build
outputs. Tests for this boundary must assert both evidence retention and the
post-run Git state; a helper-only restoration test is insufficient.

## Recover one missing reviewer result

A Tests Reviewer that needs one focused result should first use the project's
harness-native selector. When its read-only sandbox cannot create the required
build state, it records one `Evidence-Command` and one candidate-relative
`Evidence-Working-Directory` as plain lines or Markdown list entries instead of
asking a Writer to change unrelated source. Keep the command to one direct
harness invocation and place explicit build/cache arguments under
`$FLUENT_EVIDENCE_SCRATCH`.

`fluent attempt evidence run` is the human authorization boundary for that
model-proposed command. It persists a typed verification Task. The Task worker
creates private scratch before applying the sandbox, keeps candidate source
read-only, removes provider credentials from the child environment, and redirects
both shell and Core Foundation user-home caches. It retains bounded logs and
publishes a compact candidate-bound receipt. The Attempt loop validates the
receipt before using evidence attachment to rerun only the Tests Reviewer.
Scheduling must never authorize an unapproved model-proposed request implicitly.
Keep selectors harness-native instead of adding project-specific subset registries.

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

A successful test exit after this skip does not satisfy a Work Item that requires
observed confinement. Keep that obligation pending and retain the diagnostic;
neither rendered grants nor synthetic provider runs prove kernel enforcement.
For `declared_sandbox_input_reads_exact_writer_recovery`, acceptance requires
the real provider to read the exact audit bytes and fail to read the unrelated
sibling, with no confinement-unavailable early return.

Retain the Tests Review's exact evidence request against the current candidate
for an authorized environment that can apply the production profile. A Writer
must not launch host evidence work or disable worker sandboxing to obtain it.

## Patterns

- [confirm-native-tmux-detach](testing/patterns/confirm-native-tmux-detach.md) — read when a native tmux detach returns a nonzero client status in a PTY fixture.

- [joint-tla-boundary-model](testing/patterns/joint-tla-boundary-model.md) — read when connecting a workflow action to an existing detailed TLA protocol.

- [snapshot-split-work-state](testing/patterns/snapshot-split-work-state.md) — read when tracing Work Item readiness across the split Merge Candidate store.

- [test-child-owned-launch-receipts](testing/patterns/test-child-owned-launch-receipts.md) — read when a worker must prove launch identity survives its parent and serializes cancellation through exec

- [diagnose-inherited-test-pipes](testing/patterns/diagnose-inherited-test-pipes.md) — read when concurrent nextest runs report LEAK despite settled application workers

- [dispatch-recovery-through-public-cli](testing/patterns/dispatch-recovery-through-public-cli.md) — read when queue recovery must use real claim and Attempt execution without requiring nested Seatbelt.

- [pinned-older-reader](testing/patterns/pinned-older-reader.md) — read when a schema change requires proof that an older executable refuses mutation.
- [observe-live-protected-profile](testing/patterns/observe-live-protected-profile.md) — read when a host check must attempt to change an ephemeral profile while a sandboxed command runs.
- [published-release-predecessor](testing/patterns/published-release-predecessor.md) — read when upgrade tests require the exact published executable independently of the installed CLI.
- [flock-lease-tests-under-libtest](testing/patterns/flock-lease-tests-under-libtest.md) — read when writing unit tests for `lease`-based singletons that must also pass under `cargo test` (libtest threads), not just nextest.
- [unique-ids-for-attempt-worktrees](testing/patterns/unique-ids-for-attempt-worktrees.md) — read when a binary test drives a real Attempt or `fluent scheduler run`, so its sibling worktrees in the shared temp root do not collide across runs.
- [wait-for-scheduler-startup](testing/patterns/wait-for-scheduler-startup.md) — read when a scheduler test times out before preflight, queue rejection, or its first dispatch.
- [inject-stage-failure-via-filesystem-obstruction](testing/patterns/inject-stage-failure-via-filesystem-obstruction.md) — read when a test must fail one stage of a journaled pipeline (Observation/Work/queue) while earlier stages and the outer land succeed.
- [observe-sandboxed-learner-via-transcript](testing/patterns/observe-sandboxed-learner-via-transcript.md) — read when a binary test drives a sandboxed handoff-only Learner and needs to observe its run count, prompt, or commit, which it cannot record in shared-temp files.
- [reproduce-console-pipe-pressure](testing/patterns/reproduce-console-pipe-pressure.md) — read when a binary test must prove an unread or saturated writer console cannot stall transcript capture.
- [stateful-fake-cli-for-orchestration-tests](testing/patterns/stateful-fake-cli-for-orchestration-tests.md) — read when testing an operator shell script that sequences `fluent` commands and must be verified without a model, network, or the real home.

- [verify-cancellation-before-fixture-cleanup](testing/patterns/verify-cancellation-before-fixture-cleanup.md) — read when verifying cancellation during setup, child launch, or process settlement.

- [test-terminal-transport-with-confined-clients](testing/patterns/test-terminal-transport-with-confined-clients.md) — read when a transport test needs real runtime admission but the Writer sandbox denies Unix socket binds.

- [dashboard-partial-input-handoff](testing/patterns/dashboard-partial-input-handoff.md) — read when changing the Dashboard/full-screen input owner or testing input after a view handoff.
