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

The shared Crossterm reader opens the actual terminal device independently for
nonblocking input and leaves standard-stream flags unchanged. Resolve its name
before opening: macOS kqueue rejects the `/dev/tty` alias. Keep this input owner
alive across Dashboard/full-screen views. The raw-mode guard clears only an
unfinished native sequence when the display ends, then restores terminal modes.
The local Crossterm patch also terminates unsupported private CSI reports; file
flags alone do not prevent such a report from swallowing later detach keys.
This isolates file-status flags; it does not remove output backpressure.

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
