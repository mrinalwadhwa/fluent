# Wait for scheduler startup before checking recovery

## Context

Use this pattern when a scheduler integration test waits for host preflight,
queue rejection, or its first dispatch. The coordinator hashes its executable
before election and preflight. Parallel debug tests can spend their short
assertion deadlines in that setup while stderr remains empty.

## Mechanism

Give startup a bounded allowance separate from the behavior being asserted.
The binary fixtures use `SCHEDULER_STARTUP_TIMEOUT` (60 seconds), consistent
with the queue trace's Writer-start wait. Stop waiting if the child exits,
retain stderr and exit status, and settle the owned process on failure.
Observe natural exit before cleanup when exit itself proves rejection.

Keep the rejection checks: the expected diagnostic, no provider launch,
unchanged queued dispatch and Tasks, or released leases and fenced generations.
Do not extend cancellation or post-readiness exit deadlines to hide a failure.
Use the configured `FLUENT_BIN_OVERRIDE` executable for both setup and scheduler
launches, falling back to Cargo's binary only when no override was supplied.

To reproduce slow startup, delay the fixture's process before it execs the real
coordinator. A Python `time.sleep` followed by `os.execv` keeps the same PID and
process group, so fixture cleanup cannot orphan a separate sleep process.
The delayed case must cross the old deadline and still satisfy every original
rejection or recovery assertion.

## Example

`tests/binary.rs::scheduler_fixture_command` supplies the optional delay.
`scheduler_checks_host_sandbox_before_claiming_after_slow_startup` verifies
sandbox refusal before an Attempt or queue claim exists.
`host_plan_recovery_public_commands_recheck_after_slow_startup` delays the dirty
candidate case and still requires rejected evidence, no provider launch, and
unchanged queued Work.
