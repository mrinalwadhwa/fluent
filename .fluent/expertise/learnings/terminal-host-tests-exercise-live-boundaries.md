---
name: terminal-host-tests-exercise-live-boundaries
description: Prove managed terminal attachment and settlement with live PTY clients, tmux, and macOS sandbox behavior
metadata:
  type: testing
---

Helper tests for descriptor fields, reason strings, process tables, or rendered
Seatbelt text cannot establish that a managed worker is attachable, confined,
or fully settled. The terminal host check drives the public attachment command
through a PTY, sends input through real tmux clients, and checks that disconnect
preserves the worker while an occupied Control lease blocks input. Run it with
an interactive `TERM` and retain client output and exit status for connection
failures.

For cancellation, use real sandboxed pane commands and assert both allowed and
denied effects. Include descendants that change process groups, become
reparented, enter a new session, clear their environment, and close inherited
handles; also assert that unrelated host and peer processes are excluded. The
ordinary focused suite may skip these host cases, so candidate-bound macOS
evidence is necessary for the terminal confinement and settlement claims.

Related: [[external-adapter-contract-tests-drive-real-protocol]],
[[sandbox-tests-assert-invariant-in-both-host-branches]].
