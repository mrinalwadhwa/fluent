---
name: terminal-control-lease-follows-client
description: Keep Control in writable tmux clients and retain the SSH helper's lease across display handoffs
metadata:
  type: architecture
---

Managed terminal attachment observes by default. A Control attachment may
send input only while it holds the exclusive process lease. The writable tmux
client itself must inherit that lease descriptor, so the lease remains held if
the Fluent CLI exits while the client is attached. A waiting shell wrapper is
insufficient: it may die while its writable child remains connected and allow a
second controller. Control becomes available only after the attachment and its
writable clients release their copies; detaching never cancels the worker.

For SSH, the remote helper retains one runtime `Attachment` across embedded and
full-screen views. Repaint replaces only the tmux display client while the
helper keeps the same Control lease. Recreating the helper or releasing Control
during that handoff would introduce a gap in which another client could acquire
Control. Native full-screen detach returns to the embedded view through that
same helper and preserves the mode.

Reconnect starts a new helper in Observe and revalidates the saved descriptor,
including its execution generation. It requires an explicit new Control request
and never follows a replacement worker. Keep display handoff and reconnect as
distinct operations. `ssh_fullscreen_handoff_keeps_remote_control_lease` in
[`tests/ssh_agent_sources.rs`](../../../tests/ssh_agent_sources.rs) covers the
handoff, identity, and competing-controller boundary.

Related: [[managed-terminal-receipt-precedes-publication]],
[[terminal-input-flags-use-independent-description]].
