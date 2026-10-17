# Confirm native tmux detach before accepting its status

## Context

Use this in real PTY fixtures when tmux reports status 1 after Ctrl-B d despite
the server recording a native detach, as observed with tmux 3.5a.

## Mechanism

Register a `client-detached` hook on the fixture's private server before sending
the native detach chord. Require the server-side driver to observe that hook,
an empty attached-client list, a live session, and the expected final pane size.
Join the driver successfully before accepting client status 0 or 1. A failed
driver, attachment error, or other status must still fail the test; cleanup's
fallback `detach-client` command must never count as a successful native detach.

## Example

`fullscreen_attachment_pty_child` in `tests/dashboard_terminal.rs` checks that
Observe leaves the worker at 80x24 and Control changes it from 100x30 to 73x13.
It confirms native detach and retained size before accepting the client status.
`fullscreen_resize_requires_control` attempts both modes under a real PTY even
when one fails, preserving diagnostics for each.
