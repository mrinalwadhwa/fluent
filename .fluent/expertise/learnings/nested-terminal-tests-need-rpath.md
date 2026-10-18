---
name: nested-terminal-tests-need-rpath
description: Embed the prepared Ghostty library path in test-support executables that tmux launches
metadata:
  type: testing
---

The terminal test fixture starts a nested Rust test executable through tmux.
On macOS, the `DYLD_LIBRARY_PATH` set by the Tester command does not reliably
reach that nested process, so an executable linked to `libghostty-vt.dylib`
needs an `LC_RPATH` to the prepared native library directory. `build.rs` adds
that path for test-support builds. Keep a child-process regression that removes
both DYLD loader overrides and checks the executable still loads; verify the
tmux route in host evidence as well. Related: [[terminal-host-tests-exercise-live-boundaries]].
