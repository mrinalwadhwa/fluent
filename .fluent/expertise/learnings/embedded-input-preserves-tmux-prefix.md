---
name: embedded-input-preserves-tmux-prefix
description: Keep embedded Ctrl-B quoting coupled to managed tmux bindings and paste settings while preserving native full-screen detach
metadata:
  type: architecture
---

The embedded terminal writes encoded input through an ordinary tmux client.
tmux can consume a key before the worker receives it: its default Ctrl-B prefix
would turn a following `d` into detach. The adapter in
[`terminal_view/client.rs`](../../../src/terminal_view/client.rs) quotes native
Ctrl-B using the managed server's default Ctrl-B Ctrl-B send-prefix binding.
[`terminal_sessions.rs`](../../../src/terminal_sessions.rs) disables timing-based
paste detection with `assume-paste-time=0` so rapid input preserves that binding.
Change these two sides together if the managed server's input policy changes.

Keep plain paste and bracketed paste distinct. Plain paste must quote embedded
prefix bytes; bracketed paste uses its explicit boundaries to bypass tmux key
bindings. Full-screen attachment retains tmux's ordinary detach binding, and an
outer tmux has its own prefix routing. Embedded quoting must not change those
full-screen controls.

The adapter regression `embedded_input_quotes_tmux_prefix_without_changing_paste`
covers encoding modes. The real worker-byte journey
`focused_input_reaches_only_selected_invocation` and the native detach journey
`fullscreen_resize_requires_control` in
[`tests/dashboard_terminal.rs`](../../../tests/dashboard_terminal.rs) protect
delivery and detach across the tmux boundary.

Related: [[terminal-host-tests-exercise-live-boundaries]].
