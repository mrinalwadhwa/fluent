# Bound native terminal input on Crossterm 0.28.1

This directory retains the crates.io `crossterm` 0.28.1 package under its MIT
license. Its original package SHA-256 is
`829d955a0bb380ef178a640b91779e3987da38c9aea133b20614cfed8cdea9c6`.
Cargo uses this local patch for both Fluent and Ratatui, keeping one reader.
The version and key encoders remain unchanged. Cache bookkeeping files are
omitted; Git normalizes line endings and Markdown trailing whitespace.

The captured native terminal report `ESC[?2026;0$y` reaches the private CSI
parser, which previously waited only for u or c and swallowed later detach
keys. The Mio source also repeatedly read blocking inherited stdin after no
event was parsed, exceeding even a zero polling timeout.

The local patch changes these boundaries:

- `src/terminal/sys/file_descriptor.rs` opens event input through the actual
  terminal device with an independent nonblocking file description. Resolve
  the device name because macOS kqueue rejects the `/dev/tty` alias. Leave
  mode operations and inherited stdin/stdout flags unchanged. The manifest
  enables existing Rustix fs/procfs features to resolve and open that device.
- `src/event/source/unix/{mio,tty}.rs` uses that input description. The Mio
  drain stops on EOF and propagates hard read errors. Its existing parser
  lets C0 commands interrupt an unfinished CSI, while preserving paste data.
- `src/event/sys/unix/parse.rs` discards unsupported private CSI reports at
  their final byte, while retaining the existing u and c decoders.
- `src/event.rs`, `src/event/read.rs`, and `src/event/source.rs` expose
  `discard_pending_sequence` through the same synchronous reader. The Unix
  sources clear only their incomplete byte buffer; queued events remain.
  Fluent calls it when its native display guard releases raw mode, so a
  failed client cannot leave its unfinished sequence consuming Dashboard keys.

The existing root tests `fullscreen_handoff_keeps_dashboard_responsive_after_partial_input`
and `ssh_fullscreen_partial_input_returns_to_dashboard` exercise this patch
through controlling PTYs, the production handoff and helper, real lease and
generation checks, and the persistent Dashboard event dispatcher. They cover
exact and fragmented reports, incomplete CSI input, unaffected controls,
Observe/Control, normal/failure returns, and complete terminal restoration.
The root canonical Rust command discovers both tests. Real tmux/provider host
acceptance and the unchanged canonical Tester remain separate requirements.
