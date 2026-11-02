---
name: reaped-coordinator-probes-require-absent-group
description: After reaping an upgrade fixture coordinator, probe its group without signaling and require absence to prove settlement
metadata:
  type: testing
---

The integrated-upgrade fixture's coordinator cleanup relies on an unreaped child
to pin process-group identity. Once the child is reaped, its numeric group ID
provides no authority to send termination signals. Cleanup may only probe that
ID with signal zero; successful child reaping alone does not prove that the
group has disappeared.

Darwin can briefly deny that read-only probe while exited group members
disappear. Retry a denied probe within a bounded monotonic deadline, but require
`ProcessLookupError` to credit group settlement. Persistent `PermissionError`
leaves settlement unconfirmed, and a successful probe means the group still
exists. Neither result authorizes signaling a reaped coordinator's group.

Keep deterministic tests for both denial followed by absence and denial through
the deadline. Also exercise the denied probe through real foreground Attempt
cancellation, checking durable cancellation and released execution ownership
before fallback cleanup can hide a failure. The fixture boundary lives in
`tests/fixtures/integrated-upgrade.py`.

Related: [[release-fixture-process-group-ownership]],
[[process-inventory-distinguishes-exit-from-unavailable]],
[[prove-settlement-path-not-just-outcome]].
