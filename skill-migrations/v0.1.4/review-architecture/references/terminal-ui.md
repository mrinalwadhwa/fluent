# How to build terminal user interfaces

## How to test terminal UIs

Terminal UIs are hard to test because they render to a terminal device, handle input events, and manage screen state. The key insight is to separate what you render from where you render it.

### In-process rendering

Render to a virtual terminal buffer instead of a real terminal. The test constructs the application state, calls the render function with a test backend, and asserts on the buffer contents.

This works for any framework that supports pluggable backends. In ratatui, use `TestBackend`. In other frameworks, look for an equivalent: a backend that writes to memory instead of stdout.

```rust
let backend = TestBackend::new(80, 24);
let mut terminal = Terminal::new(backend).unwrap();
terminal.draw(|f| draw_my_widget(f, f.area(), &state)).unwrap();
let text = buffer_text(terminal.backend().buffer());
assert!(text.contains("expected content"));
```

Build helper functions early:
- `buffer_text(buffer) -> String` — extract all text from the buffer as a single string for simple assertions
- `cell_at(buffer, row, col) -> &Cell` — inspect individual cells for style, color, or character checks
- `has_style(buffer, text, style) -> bool` — verify that specific text appears with the expected styling

### What to assert on

Test observable properties, not pixel positions:
- Text content appears somewhere on screen
- Status labels reflect the current state
- Active elements are visually distinct from inactive ones
- Content wraps correctly at terminal width boundaries
- Border characters are only in border positions

Avoid asserting on exact row/column positions unless the layout is the thing being tested. Content shifts when the terminal resizes or when surrounding content changes.

### Testing animation

Animation cycles through frames on each render tick. Don't assert on specific frame characters — they depend on timing. Instead:
- Assert the animation indicator is present when active
- Assert it's absent when inactive
- Assert different ticks produce different output (the animation advances)

```rust
let frame_0 = render_at_tick(&state, 0);
let frame_1 = render_at_tick(&state, 1);
assert_ne!(frame_0, frame_1);  // animation advances
```

### Testing state transitions

Construct state A, render, assert. Mutate to state B, render again, assert the change is reflected. This catches stale rendering — fields that don't update when the underlying state changes.

```rust
let text_1 = render(&state_running);
assert!(text_1.contains("Running"));

let text_2 = render(&state_complete);
assert!(text_2.contains("Complete"));
assert!(!text_2.contains("Running"));
```

### Full-process testing with tmux

For integration tests that need the real binary running in a real terminal, use tmux:

1. Start a tmux session with the application
2. Wait for expected content with `tmux capture-pane -p`
3. Send keystrokes with `tmux send-keys`
4. Capture and assert on the screen output
5. Clean up the session

```bash
tmux new-session -d -s test -x 80 -y 24 -- my-app
sleep 1
tmux capture-pane -t test -p > /tmp/capture.txt
grep -q "expected content" /tmp/capture.txt
tmux kill-session -t test
```

Use a polling helper that retries capture until expected content appears or a timeout is reached. Don't rely on fixed sleeps.

```rust
fn wait_for(pane: &str, needle: &str, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let capture = capture_pane(pane)?;
        if capture.contains(needle) {
            return Ok(capture);
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for {needle:?}");
        }
        sleep(Duration::from_millis(100));
    }
}
```

Mark tmux tests as ignored by default — they require tmux installed and are slower than in-process tests. Run them manually or in CI with tmux available.

### Testing keyboard navigation

For in-process tests, simulate input by mutating state directly (call the handler that a keypress would trigger, then re-render). For tmux tests, use `tmux send-keys` to send actual keystrokes and capture the result.

### Testing with multi-byte and styled content

Terminal UIs must handle:
- Multi-byte UTF-8 characters (CJK characters are 2 columns wide)
- ANSI escape sequences from external command output
- Emoji and combining characters

Write tests with content containing these. Assert that:
- All characters appear in the rendered output
- Wide characters don't cause content to shift or truncate
- ANSI sequences are stripped or rendered correctly
- Border characters remain intact

### Snapshot testing

Capture the rendered buffer as a string and compare against a saved snapshot. This catches unintended visual regressions. Use a snapshot testing library (insta for Rust, Jest snapshots for JS) or a simple file comparison.

Snapshots complement behavioral assertions — they don't replace them. A snapshot tells you something changed. A behavioral test tells you whether the change is correct.

### Structuring test helpers

Keep rendering code testable by separating it from the event loop. The render function should take state in and produce frames out, with no side effects. The event loop handles input and timing. Tests call the render function directly.

Extract reusable test helpers into a shared module:
- State builders that construct test scenarios
- Render helpers that draw a widget and return the buffer
- Assertion helpers for common checks
