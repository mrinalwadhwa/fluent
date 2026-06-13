2026-06-05 — The factory should be able to visually observe terminal
UIs during testing. Launch the dashboard (or any TUI) in a tmux
session, capture the screen with tmux capture-pane, and evaluate
the rendered output. This enables autonomous agents to catch
visual bugs (missing animation, stale status, rendering glitches)
without a human looking at screenshots. This should be a skill —
distributable expertise on how to test terminal user interfaces
using tmux capture and VT100 rendering.
→ Resolved: added the `test-terminal-ui` skill, backed by
`expertise/terminal-ui.md`, to package in-process render testing and
tmux capture as a reusable workflow.
