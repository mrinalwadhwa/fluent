# Keep partial input in one reader through a view change

## Context

Use this regression when changing Dashboard full-screen input. Crossterm may
read a view-switch key and the beginning of another terminal sequence in the
same read. A second reader can consume the remaining bytes and leave the first
reader blocked after the view returns, even when its poll requests a timeout.

## Mechanism

Drive a real controlling PTY with an independent watchdog. Send `f` and a
partial SGR mouse sequence together before the Dashboard reads its first event.
Send the mouse suffix only after full-screen paint confirms entry. Return with
native detach or a client failure, then send ordinary errors-overlay and
selection keys. Assert the real Dashboard state changes and exact terminal
flags/control characters return. Include a control without the partial prefix.
Do not send parser-recovery bytes after return. Keep external-client doubles
at the existing transport seam; retain real tmux and provider host gates.

## Example

`fullscreen_handoff_keeps_dashboard_responsive_after_partial_input` uses
`dashboard-partial-handoff.py` for local Observe/Control and normal/failure
returns. `ssh_fullscreen_partial_input_returns_to_dashboard` uses the existing
SSH helper fixture. Both call `assert_handoff_navigation` against the production
Dashboard state machine after returning. These reproduce an established split
reader failure path; they do not establish the exact original host bytes.

## Exercise native terminal reports

Also send the valid automatic terminal-mode report `ESC[?2026;0$y` after
native paint, both together and fragmented across reads, before native detach.
Include separated Ctrl-B and d, an unfinished CSI without its suffix, and a
paste containing control bytes. Assert Observe sends no client input and
Control retains paste content through the native encoder. Keep the same
Dashboard reader alive through return and require errors-overlay and selection
changes without recovery bytes. Compare every termios flag and control
character after the client exits.

The input relay in the retained diagnostic captured that exact report before
an actual Observe detach failure. It establishes that reproduced mechanism;
it does not identify the bytes in the earlier unlogged host interval. The
focused OpenSSH/tmux doubles exercise the real helper and lease boundary;
fresh real-provider host acceptance remains separate.
