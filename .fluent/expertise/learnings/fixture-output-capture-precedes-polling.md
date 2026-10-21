---
name: fixture-output-capture-precedes-polling
description: Capture scheduler fixture output while polling progress so unread pipes cannot block completion or exit
metadata:
  type: testing
---

Scheduler fixtures in `tests/binary.rs` poll durable dispatch state or child
exit before collecting diagnostics. Piped stdout or stderr left unread during
that poll can fill and block the child before it publishes completion or exits.
Calling `wait_with_output` after the poll then releases the blockage, so a
timeout report can contain successful completion output produced too late to
satisfy the assertion.

Use `OwnedFixtureProcess::spawn_with_output` for these polling fixtures. It
redirects both streams to temporary files before spawn and returns their bytes
through `wait_with_output` after process-group cleanup. Keep the capture
directory alive through collection and retain the existing polling deadline,
state assertions, and process-group owner. A fixture intentionally testing pipe
pressure still needs its explicit pressure setup and bounded cleanup.

Verify capture through the shared fixture helper with output larger than pipe
capacity. Require completion publication before collection in the successful
case and natural nonzero exit before collection in the failure case; then
assert the complete stdout and stderr bytes. This separates child progress from
cleanup or late output draining that could otherwise hide the blockage.

Related: [[release-fixture-process-group-ownership]],
[[capture-pump-terminate-descendants-before-eof]],
[[prove-settlement-path-not-just-outcome]].
