# How to test child-owned launch receipts

## Context

Use this pattern when a parent records process ownership after creating a child,
and recovery must still identify the child if the parent dies during launch.

## Mechanism

Reserve a fresh generation under the execution lock. Make the child acquire its
inherited identity lock and write a fixed receipt containing its PID before exec.
Hold the parent model and execution lock descriptors across fork, so the child
keeps them until exec even if the parent dies. Test the transition with separate
processes: stop the child after receipt publication, kill the parent, request a
durable stop in another process or thread, and verify that stop waits until the
child crosses exec. Test malformed receipts while the identity lock stays held;
neither cancellation nor a replacement launch may infer a group from them.

## Example

`tests/execution_handoff.rs::launch_handoff_cancel_serializes_with_payload_start`
holds a test-only pre-exec barrier after receipt publication. It kills the
launch parent, observes the Work model lock blocking a stop mutation, and
releases the child. When the stop mutation obtains the model lock, it checks
both the child's final pre-exec marker and the operating system's executable
path for the child PID. The stop commits without waiting for a payload action.
The test then verifies the payload ran and cancellation settled. Inspecting the
executable at lock acquisition detects a stop that overtakes exec even if a
later payload action succeeds.

For a public Task route, setup commands share the Task execution record. Set
`FLUENT_TEST_LAUNCH_BARRIER_PROGRAM` to the provider executable basename when
the observation must interrupt that provider rather than an earlier setup
child. Wait for `<barrier>.ready` before reading the complete receipt. Match a
PID written by the payload with the receipt PID, then verify cancellation and
lock release before dropping any failure-cleanup guard.

When a Task reuses an artifact area, preparation may write a new launch
generation while the prior child's receipt remains. To prove that the new
generation stopped before spawn, compare the execution record's generation
with a strictly parsed receipt. A well-formed receipt for a different
generation can remain as historical evidence; a current-generation or
malformed receipt blocks recovery. Bind the retained receipt bytes in the
recovery audit and test both the public retry and a changed receipt after
approval. `no_change_completion_retry_resumes_saved_prelaunch_failure` and
`no_change_completion_retry_binds_retained_receipt` exercise those boundaries.
