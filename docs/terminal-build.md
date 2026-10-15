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
cargo build --offline
```

Preparation downloads verified archives and builds the static library under
`target/terminal-deps/install/<Rust target>`. Its Zig package and compiler
caches remain under `target/terminal-deps/`. Cargo's build script compiles the
local adapter and links the prepared archive; it never downloads dependencies.
Keep the Attempt-provided `CARGO_HOME` when working in a Fluent workspace.

To prepare the other macOS release target, pass `x86_64-apple-darwin` or
`aarch64-apple-darwin` to the same script before `cargo build --target`.
Installed Fluent executables contain the static library. Operators need tmux,
but do not need Zig or a separate Ghostty installation.

In the dashboard, expand a Work Item, select its native agent worker, and press
`t` to observe its terminal beside the project list. Observe clients use tmux's
`read-only,ignore-size` flags. Terminal output is interpreted inside Ghostty
and projected as cells; it cannot set the dashboard title or clipboard.
