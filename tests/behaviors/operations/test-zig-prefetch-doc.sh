#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../../.." && pwd)
doc="$root/.fluent/expertise/testing/native-deps/patterns/prefetch-verified-zig-packages.md"
[[ $(head -n 1 "$doc") == '# How to '* ]]
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/tools" "$scratch/bin" "$scratch/target/terminal-deps"
cp "$root/tools/terminal-deps.env" "$scratch/tools/terminal-deps.env"
source "$scratch/tools/terminal-deps.env"
cache="$scratch/target/terminal-deps"
mkdir -p "$cache/ghostty-$GHOSTTY_REVISION" "$cache/zig-aarch64-macos-$ZIG_VERSION"

cat > "$scratch/bin/curl" <<'SH'
#!/bin/sh
set -eu
while [ "$#" -gt 0 ]; do
  if [ "$1" = -o ]; then
    printf archive > "$2"
    exit 0
  fi
  shift
done
exit 1
SH
cat > "$cache/zig-aarch64-macos-$ZIG_VERSION/zig" <<'SH'
#!/bin/sh
set -eu
[ "$1" = fetch ]
[ "$2" = --global-cache-dir ]
[ "$3" = "$FLUENT_DOC_TEST_ROOT/target/terminal-deps/zig-cache" ]
[ -f "$4" ]
touch "$FLUENT_DOC_TEST_ROOT/fetched"
SH
chmod +x "$scratch/bin/curl" "$cache/zig-aarch64-macos-$ZIG_VERSION/zig"

awk '/^```sh$/ { in_block=1; next } in_block && /^```$/ { exit } in_block { print }' \
  "$doc" \
  > "$scratch/example.sh"
test -s "$scratch/example.sh"
(
  cd "$scratch"
  FLUENT_DOC_TEST_ROOT="$scratch" PATH="$scratch/bin:$PATH" bash "$scratch/example.sh"
)
test -f "$scratch/fetched"
