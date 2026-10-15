#!/bin/bash
# Acquire verified source inputs explicitly; Cargo never downloads native code.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
source "$root/tools/terminal-deps.env"
cache="$root/target/terminal-deps"
mkdir -p "$cache"
case "$(uname -m)" in
  arm64) host=aarch64; zig_hash=$ZIG_AARCH64_MACOS_SHA256 ;;
  x86_64) host=x86_64; zig_hash=$ZIG_X86_64_MACOS_SHA256 ;;
  *) echo 'Terminal dependency preparation requires a macOS release host.' >&2; exit 1 ;;
esac
[[ $(uname -s) == Darwin ]] || { echo 'macOS is required.' >&2; exit 1; }
target=${1:-$host-apple-darwin}
case "$target" in
  aarch64-apple-darwin) zig_target=aarch64-macos ;;
  x86_64-apple-darwin) zig_target=x86_64-macos ;;
  *) echo "Unsupported terminal target: $target" >&2; exit 1 ;;
esac
fetch() {
  local url=$1 destination=$2 expected=$3
  if [[ ! -f "$destination" ]]; then
    curl --fail --location --silent --show-error "$url" -o "$destination.part"
    mv "$destination.part" "$destination"
  fi
  local actual
  actual=$(shasum -a 256 "$destination")
  [[ ${actual%% *} == "$expected" ]] || { echo "Checksum mismatch: $destination" >&2; exit 1; }
}
fetch "https://codeload.github.com/ghostty-org/ghostty/tar.gz/$GHOSTTY_REVISION" "$cache/ghostty.tar.gz" "$GHOSTTY_SHA256"
fetch "https://ziglang.org/download/$ZIG_VERSION/zig-$host-macos-$ZIG_VERSION.tar.xz" "$cache/zig-$host.tar.xz" "$zig_hash"
source_dir="$cache/ghostty-$GHOSTTY_REVISION"
zig_dir="$cache/zig-$host-macos-$ZIG_VERSION"
[[ -d "$source_dir" ]] || tar -xzf "$cache/ghostty.tar.gz" -C "$cache"
[[ -d "$zig_dir" ]] || tar -xJf "$cache/zig-$host.tar.xz" -C "$cache"
[[ $("$zig_dir/zig" version) == "$ZIG_VERSION" ]]
# Zig verifies every transitive package against upstream build.zig.zon hashes.
# Keep both compilation and package caches inside the assigned checkout.
cd "$source_dir"
"$zig_dir/zig" build -Demit-lib-vt=true -Demit-xcframework=false \
  -Doptimize=ReleaseFast -Dsimd=false -Dtarget="$zig_target" \
  --global-cache-dir "$cache/zig-cache" --cache-dir "$cache/zig-local-$target" \
  --prefix "$cache/install/$target"
printf '%s\n' "$GHOSTTY_REVISION $GHOSTTY_SHA256 $ZIG_VERSION $target" > "$cache/install/$target/fluent-inputs"
shasum -a 256 "$cache/install/$target/lib/libghostty-vt.a" | cut -d ' ' -f 1 >> "$cache/install/$target/fluent-inputs"
