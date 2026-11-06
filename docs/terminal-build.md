# Build the embedded terminal

Fluent embeds the MIT-licensed libghostty-vt C library from upstream
`ghostty-org/ghostty` at revision
`44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51`.
`tools/terminal-deps.env` pins the source archive SHA-256 and Zig 0.16.0
release archive checksums. The upstream `build.zig.zon` pins transitive
packages by content hash. Fluent owns its small C/Rust adapter; it does not
depend on the unpublished Herdr Rust crate.

Before building from source, run:

```sh
bash scripts/prepare-terminal-deps.sh
cargo fetch --locked
cargo build --offline
```

Preparation downloads verified archives and builds the static library under
`target/terminal-deps/install/<Rust target>`. Its Zig package and compiler
caches remain under `target/terminal-deps/`. Cargo's build script compiles the
local adapter and links the prepared archive; it never downloads dependencies.
Keep the Attempt-provided `CARGO_HOME` when working in a Fluent workspace.
The preparation script does not populate Cargo's registry or crate cache;
`cargo fetch --locked` fills that cache before the first offline build. It
needs network access unless the selected Cargo cache already has every locked
dependency.

To prepare the other macOS release target, pass `x86_64-apple-darwin` or
`aarch64-apple-darwin` to the same script before `cargo build --target`.
Installed Fluent executables contain the static library. Operators need tmux,
but do not need Zig or a separate Ghostty installation.

The dashboard groups Work by project. Use Tab/Shift+Tab to cycle Current, Needs you,
Running, Ready, Waiting, History, and All. Current opens first and excludes only
History. Ready includes Ready to land; Waiting
includes Queued and Planned. Each row keeps its exact status. The Updated UTC
column uses the latest published transition (or recorded task start/completion)
and shows `—` when no timestamp is available, including the current SSH snapshot.
The `f`/`F` filter aliases and `a` History/All shortcut remain available when
navigation owns focus; with a terminal open, lowercase `f` opens full-screen.

Use Up/Down to select a Work Item and Enter to expand or collapse its workers.
Right opens labeled Work Item or worker details across the full body. Press `t` explicitly to
open the selected worker terminal in Observe or reconnect its existing view. Moving selection releases the previous
attachment and waits for an explicit open command before attaching elsewhere.
Left/Esc returns to the list at every supported width. Tab and
Shift+Tab keep switching views in either pane while navigation owns focus. Observe clients use tmux's
`read-only,ignore-size` flags and preserve the worker's dimensions even when
no controller is attached. Only Control resizes the worker; releasing Control
retains its last dimensions. Terminal output is interpreted inside Ghostty
and projected as cells; it cannot set the dashboard title or clipboard.

Press `i` to acquire Control. While the terminal has focus, Esc, Ctrl-C,
keys, mouse events inside the viewport, and paste reach that invocation.
Ctrl-C does not cancel the Attempt. Press Ctrl-] to return to dashboard
navigation while retaining Control; press `o` to release Control and observe.
Ctrl-5 also returns to navigation: the terminal decoder treats it as the
legacy Ctrl-] character.
To send the reserved character literally, press Ctrl-], then `]`; this sends
byte 0x1d and restores terminal focus. Paste is limited to 64 KiB per event.

The terminal header wraps long invocation identities so the Task and generation
stay visible. Shrinking the dashboard below 60 columns or 15 rows returns input
to dashboard navigation and preserves the worker's dimensions. After enlarging
the window, press `i` to focus the terminal again; focus does not return on resize.

Press `f` from navigation with a terminal open to attach full-screen in the
current Observe or Control mode. Fluent settles the embedded client before
transferring the same lease to the inherited-terminal client. Use tmux's
native detach (`Ctrl-b`, then `d`) to return. Fluent restores its modes,
selection, and current dimensions after detach, worker exit, or failure.
Use `F` to change the Work filter while a terminal is open.

After a display disconnect, `t` creates a fresh parser and tmux client to
request a complete repaint of the same invocation. An ended invocation keeps
its final view and cannot regain Control; explicitly select another worker.
Inside an outer tmux, the outer prefix still belongs to the outer server:
use its send-prefix binding (normally `Ctrl-b Ctrl-b`) before `d` to detach
the inner full-screen client. The required host acceptance checks this nested
routing with real providers.

The display reader drains a nonblocking PTY independently of navigation and
Work polling. It reads at most 8 KiB per iteration, publishes one replaceable
frame at most every 33 ms, and retains no local scrollback. Viewports are
limited to 500 columns by 200 rows; the input queue holds at most sixteen
bounded events. Unsupported input, parser snapshot failures, and reply-buffer
overflow show a diagnostic with a reconnect path. The runtime still owns
worker execution and provider evidence.

`scripts/release.sh` prepares the native library before each existing macOS
release target and includes the Ghostty license with release assets. The
operator acceptance command is `bash tests/host/check-dashboard-agent-terminal.sh`.
It requires separate host/provider authorization, an external evidence directory,
and disposable live native sources for each of Codex, Claude, and Pi (the
required environment variables are listed at the script's start). It builds
and identifies both release binaries, captures direct and nested dashboard
sessions, and records concrete operator observations for every scenario.
Incomplete scenarios fail the check; targeted Tests Review must inspect the
captures and observations. Writer checks do not execute this host journey.
