# Prefetch verified Zig packages when Zig cannot resolve a host

## Context

Use this when `scripts/prepare-terminal-deps.sh` stalls or reports
`UnknownHostName` for a dependency in a pinned `build.zig.zon`, while curl can
reach the same archive. Keep the package cache under this checkout's
`target/terminal-deps` directory.

## Mechanism

Download the declared archive with `curl --fail --location` into the checkout.
Run `zig fetch --global-cache-dir ../zig-cache <archive>` from
`target/terminal-deps/ghostty-<pinned-revision>` (or use an absolute path to
the checkout's `target/terminal-deps/zig-cache`). Compare Zig's printed content hash with the
exact `.hash` in that package's `build.zig.zon`. Repeat for nested packages
named by the next build error, then rerun `scripts/prepare-terminal-deps.sh`.

## Example

On an Apple Silicon Mac, if the build names a GitHub archive, fetch the same commit from codeload,
then ask Zig to verify and cache its content:

```sh
curl --fail --location --retry 2 \
  https://codeload.github.com/vancluever/arocc/tar.gz/f97cdfc3779aec4b242299e2fc9a1c828c3547c6 \
  -o target/terminal-deps/aro.tar.gz
cd target/terminal-deps/ghostty-<pinned-revision>
../zig-aarch64-macos-0.16.0/zig fetch \
  --global-cache-dir ../zig-cache ../aro.tar.gz
```

The printed `aro-...` hash must match the dependency declaration before the
native build resumes.
