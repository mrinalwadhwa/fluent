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
