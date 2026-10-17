# How to prefetch verified Zig packages when Zig cannot resolve a host

## Context

Use this when `scripts/prepare-terminal-deps.sh` stalls or reports
`UnknownHostName` for a dependency in a pinned `build.zig.zon`, while curl can
reach the same archive. Keep the package cache under this checkout's
`target/terminal-deps` directory.

## Mechanism

Download the declared archive with `curl --fail --location` into the checkout.
Load the pinned revision and Zig version from `tools/terminal-deps.env`, then
run `zig fetch` from that Ghostty source directory with the checkout's Zig
package cache. Compare Zig's printed content hash with the exact `.hash` in
that package's `build.zig.zon`. Repeat for nested packages named by the next
build error, then rerun `scripts/prepare-terminal-deps.sh`.

## Example

From the checkout root on an Apple Silicon Mac, if the build names a GitHub
archive, fetch the same commit from codeload, then ask Zig to verify and cache
its content:

```sh
. tools/terminal-deps.env
cache="$(pwd)/target/terminal-deps"
curl --fail --location --retry 2 \
  https://codeload.github.com/vancluever/arocc/tar.gz/f97cdfc3779aec4b242299e2fc9a1c828c3547c6 \
  -o "$cache/aro.tar.gz"
cd "$cache/ghostty-$GHOSTTY_REVISION"
"$cache/zig-aarch64-macos-$ZIG_VERSION/zig" fetch \
  --global-cache-dir "$cache/zig-cache" "$cache/aro.tar.gz"
```

The printed `aro-...` hash must match the dependency declaration before the
native build resumes.
