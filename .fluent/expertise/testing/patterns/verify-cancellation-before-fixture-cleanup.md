# Verify cancellation before fixture cleanup

## Context

Use this pattern when a CLI test must prove that cancellation stops an owned
worker tree. A terminal `execution.json` state does not prove process exit, and
an owned fixture guard can accidentally hide a leak by stopping it during
`wait` or `Drop`.

## Mechanism

Have the worker publish its PID and a long-lived descendant PID before declaring
itself ready. Capture the execution record's leader and process group too, and
check that all captured processes are live in that group before cancellation.
Give both cancellation and the Attempt runner one deadline substantially shorter
than the fixture's natural lifetime. Poll `try_wait` to observe and reap their
exit without invoking fixture cleanup.

Before dropping any guard, require `kill(pid, 0)` to report `ESRCH` for every
captured PID and acquire the existing `execution-owner.lock` through a read-only
open and a nonblocking exclusive flock. Retry the release check within a bounded
interval because macOS can briefly delay flock release visibility.

Keep a separate failure-cleanup guard for the worker group: workers leave the
CLI owner's group. Setup and baseline commands can replace the same Task's
execution record, so refresh the guard's group until the target worker reaches
its ready barrier. For CLI fixture cleanup, accept macOS `EPERM` only after
observing that the owned child exited; live descendants still require a sweep.

To verify intent ordering, let the worker's first SIGTERM handler snapshot the
persisted cancellation record, then wait at a release barrier. Assert the
snapshot before releasing the worker. Mutation-check these assertions by moving
signaling before intent or writing terminal metadata without signaling, restore
the source, then rerun the ordinary focused command.

To cancel between Tester children, set `FLUENT_TEST_TESTER_SETTLED_BARRIER` on
the fixture CLI to a private release-file path. The `test-support` build writes
the same path with a `.ready` extension after its first command settles, then
waits up to twenty seconds for the release file before publishing command logs.
Wait for that marker and confirm the settled record, released inherited
ownership, and held Task lease. Issue one cancellation and wait for the record
to become canceled, then write the release file. Require both runner and
cancellation to exit within a bounded wait, and assert that neither the next
command nor the extractor wrote its launch marker. Also require the first
command's normal output artifact, so an artifact-write failure cannot satisfy
the test. A positive control releases the same barrier without cancellation and
requires the next command and extractor to run normally.

Do not use a command-log FIFO as the barrier: the protected artifact writer
rejects non-regular files. Racing that rejection can make a broken fixture pass
before its Task changes to `needs-user`. The explicit barrier exists only in
`test-support` builds and leaves the production artifact checks unchanged.

To cancel during Learner provider-capacity waiting, persist a reviewed pending
candidate and occupy every configured provider slot using the same advisory
leases as admission. Keep shared slot files under the project's hermetic
`test-user-provider-admission` root; do not contend with the operator's pool.
Wait for the real capacity-wait diagnostic and verify the Learner lease is held
before issuing one CLI cancellation. Observe both CLI exits and Learner lease
release while every provider lease is still held. Assert settled cancellation,
no Learning reservation or child launch, and unchanged candidate bytes before
releasing the fixture's capacity. This proves that the wait itself observes the
stop; releasing slots or dropping cleanup guards cannot satisfy the test.

To cancel before the first Task reservation, implement an `AttemptRunner` fixture
that signals readiness and waits on a bounded channel before invoking the real
Attempt CLI with its dispatch ID. Drive it through `scheduler::dispatch_one`;
confirm the dispatch is running and owns its lease, cancel, and observe a settled
stop with all Tasks still planned. Release the channel and require the exact stop
to remain, no worker launch marker, and a `needs-user` dispatch. Follow with a new
public queue resume and require its worker marker as a positive control.
Bound authorized recovery setup separately: worktree preparation and baseline
extraction precede that marker and do not measure cancellation latency. Keep the
stale dispatch's original exit deadline; give only the explicitly resumed
dispatch a bounded marker wait, reject an early child exit without the marker,
and require child exit within the original deadline after the marker. Poll
`try_wait` before dropping either the CLI or nested-execution cleanup guard.
A controlled slow baseline must pass this positive control, while a missing
Writer marker, unauthorized stale launch, or non-exiting child must still fail.
Retain the diagnostic patches and native before/after output, then remove the
injected delays and instrumentation before committing.

To verify the shared Attempt stop during parallel setup, reserve two reviewers
through the real Task reservation boundary and retain both Task leases. Start A
with a SIGTERM handler that announces termination and waits for release; hold B
on a channel immediately before `execution::output`. Cancel the Attempt, wait
for A's handler, and confirm durable intent while B's record still says
`launching`. Release B and require a rejected launch and no marker while A still
holds its execution identity. Only then release A and observe final settlement.
Bound the worker's natural lifetime and channel waits so failed assertions also
unwind without leaking a worker.

## Example

`tests/binary.rs::attempt_cancel_stops_each_stage_without_downstream_launch`
captures worker and descendant identities for Writer, verification, Tester,
review, and Learner. Its eight-second cancellation deadline is shorter than the
fixtures' 120-second lifetime, and it checks OS process exit and inherited
ownership before either fixture guard can clean up.

`attempt_cancel_between_tester_commands_blocks_next_child` and
`attempt_cancel_between_tester_command_and_extractor_blocks_next_child` use the
settled-command barrier to verify both transitions after a naturally settled
Tester child. `tester_settled_command_barrier_allows_normal_completion` checks
that release without cancellation still runs the downstream children.

`attempt_cancel_during_scheduler_startup_blocks_delayed_launch` uses the channel
barrier and a real CLI child to check startup and fresh recovery authority.

`src/work_task_executor.rs::attempt_stop_blocks_reserved_peer_while_another_worker_settles`
covers the interval before cancellation visits the reserved peer's own record.

`attempt_cancel_settles_learner_waiting_for_provider_capacity` holds all provider
slots through the cancellation assertions and checks the Complete-state stop
before any Learner execution identity or Learning run can be reserved.
