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

For embedded input, send raw bytes through the production event decoder and
assert the bytes received by the actual worker. Crossterm decodes legacy 0x1d
as Ctrl-5, while enhanced input can identify Ctrl-] directly; constructing only
the latter event misses the real shortcut boundary. Drive worker selection
through the shared production dispatcher so the test observes release of the
old attachment and explicit Control of the new one. See
[[embedded-input-preserves-tmux-prefix]] and
[[extract-logic-to-avoid-test-duplication]].

The dashboard's operator-driven host script records attestations. Acceptance
also needs readable captures and concrete action-to-result observations for
each required provider and direct or outer-tmux mode on the same candidate.
An Observe resize cannot prove Control resizes the worker, and a failure of an
embedded attach cannot prove full-screen failure restoration. Preserve valid
partial proof while supplying the missing scenario; a passing script exit or
an older candidate's capture cannot fill that gap.

For cancellation, use real sandboxed pane commands and assert both allowed and
denied effects. Include descendants that change process groups, become
reparented, enter a new session, clear their environment, and close inherited
handles; also assert that unrelated host and peer processes are excluded. The
ordinary focused suite may skip these host cases, so candidate-bound macOS
evidence is necessary for the terminal confinement and settlement claims.

Related: [[external-adapter-contract-tests-drive-real-protocol]],
[[sandbox-tests-assert-invariant-in-both-host-branches]].
