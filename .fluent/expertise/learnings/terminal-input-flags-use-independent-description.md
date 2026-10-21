---
name: terminal-input-flags-use-independent-description
description: Open full-screen input separately so nonblocking reads cannot change PTY output flags
metadata:
  type: gotcha
---

Fluent's dashboard can inherit stdin and stdout from the same PTY open file
description. Setting `O_NONBLOCK` on stdin then also changes stdout, so a large
full-screen repaint can fail with `WouldBlock` and disconnect the display.
Duplicating stdin retains those shared file-status flags.

The SSH full-screen path opens `/dev/tty` independently for nonblocking input
and leaves the standard-stream flags unchanged. Keep that descriptor owned by
the raw-mode guard and preserve the dashboard's terminal restoration on return.
This isolates file-status flags; it does not give output a separate terminal
buffer or remove output backpressure.

Exercise this boundary through the dashboard's actual full-screen handoff after
its event reader consumes `f`. Withhold PTY output reads during a repaint, then
require the complete repaint before sending detach. Check Observe and Control,
unchanged stdin/stdout flags, restored dashboard modes, invocation identity,
Control exclusivity, and worker liveness. A direct attachment test or a small
repaint can miss the shared-descriptor failure. See
`ssh_fullscreen_handoff_keeps_remote_control_lease` in
[`tests/ssh_agent_sources.rs`](../../../tests/ssh_agent_sources.rs).

Related: [[terminal-session-cleanup-covers-partial-setup]],
[[terminal-control-lease-follows-client]].
