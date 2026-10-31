---
name: ssh-input-acknowledgement-does-not-prove-delivery
description: Acknowledge and discard validated SSH input after display detach while retaining the helper and Control lease
metadata:
  type: architecture
---

The SSH helper can observe native display detach before the local input owner
receives the detach response. Authorized input may still arrive during that
interval. The helper validates the invocation, Control authority, and input
queue bounds before acknowledging the request. When its display PTY is absent,
it discards those bytes while retaining the helper and its Control lease.
The acknowledgement therefore does not guarantee delivery to the worker.

Do not turn this handoff into a transport failure or retain the discarded bytes
for the next display client. Repaint opens a display for the same attachment;
fresh input can then flow through the retained Control authority. Observe and
invalid requests still fail their authority checks even when no display exists.

`ssh_native_detach_discards_inflight_input_and_retains_control` in
[`tests/ssh_agent_sources.rs`](../../../tests/ssh_agent_sources.rs) checks the
production helper's retained descriptor and Control, competitor exclusion,
discarded stale input, repaint, and successful fresh input.

Related: [[terminal-control-lease-follows-client]],
[[native-input-settlement-binds-identity]].
