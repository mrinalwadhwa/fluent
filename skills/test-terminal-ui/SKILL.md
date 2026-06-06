---
name: test-terminal-ui
description: >
  Test terminal user interfaces by rendering to in-memory terminal
  buffers when possible, or by launching the real binary in tmux and
  capturing the pane when full-process behavior matters. Use this when
  checking dashboard/TUI layout, animation, navigation, stale rendering,
  or terminal-specific behavior.
---

# Test terminal UI

Test terminal user interfaces by observing what a user would see in a
terminal. Prefer fast in-process rendering tests. Use tmux only when the
event loop, terminal mode, keyboard input, process lifecycle, or real
binary behavior is the thing under test.

---

## How to run this skill

### Phase 1 — Load the TUI expertise

Read `references/terminal-ui.md`. It covers render-buffer testing,
animation checks, state transitions, tmux capture, keyboard navigation,
wide characters, ANSI handling, snapshots, and helper structure.

Identify what the test needs to prove:
- Layout or text rendering
- Active/idle visual state
- Animation presence or advancement
- Keyboard or mouse navigation
- State refresh after files or run status changes
- Real terminal behavior from the compiled binary

### Phase 2 — Prefer in-process rendering

If the UI has a render function or test backend, write an in-process
test first:
- Construct the application state directly.
- Render into an in-memory terminal buffer.
- Assert on observable screen output, styles, or state transitions.
- Avoid exact row/column assertions unless layout positioning is the
  behavior being tested.

For animation, assert that the indicator appears when active, disappears
when inactive, or changes across ticks. Do not assert on wall-clock
timing.

### Phase 3 — Use tmux for full-process checks

Use tmux when the test must exercise a real terminal process:
- Start a session with explicit dimensions.
- Launch the binary under test.
- Poll `tmux capture-pane -p` until expected text appears or a timeout
  expires.
- Send keys with `tmux send-keys` for navigation.
- Capture the pane after each interaction.
- Kill the tmux session in cleanup.

Do not rely on fixed sleeps except for very short backoffs inside a
polling loop. A TUI test should fail with captured screen output when the
expected text never appears.

### Phase 4 — Check terminal edge cases

Include edge cases that real terminal UIs mishandle:
- Narrow and wide terminal sizes
- Long text wrapping
- Empty states
- Active and terminal statuses
- Multi-byte and wide characters
- ANSI escape sequences from command output
- Scroll position and auto-scroll behavior

### Phase 5 — Validate and report

Run the focused TUI tests and any related behavior tests. If a visual
issue was originally reported from a screenshot, include the captured
screen text or test assertion that now covers it.

If tmux is unavailable, report that clearly and still add in-process
coverage when possible.

---

## Rules

- **Observe output, not internals.** Assert on what the user can see or
  do.
- **Prefer in-process tests.** They are faster, less flaky, and easier
  to debug.
- **Use tmux deliberately.** It is for real terminal integration, not
  for every render assertion.
- **Poll, don't sleep.** Wait until expected text appears or timeout with
  useful captured output.
- **Keep cleanup reliable.** Always kill tmux sessions created by tests.
- **Cover state changes.** Render before and after status or input
  changes so stale UI state is caught.
