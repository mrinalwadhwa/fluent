#!/usr/bin/env bash
set -euo pipefail

if [[ "${TRACE-0}" == "1" ]]; then set -o xtrace; fi

command -v cargo >/dev/null 2>&1 || { printf 'error: cargo not found\n' >&2; exit 1; }
command -v gh >/dev/null 2>&1    || { printf 'error: gh CLI not found\n' >&2; exit 1; }
command -v shasum >/dev/null 2>&1 || { printf 'error: shasum not found\n' >&2; exit 1; }

readonly REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

version=$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')
if [[ -z "$version" ]]; then
  printf 'error: could not extract version from Cargo.toml\n' >&2
  exit 1
fi

readonly TAG="v${version}"
readonly TARGET_TRIPLE="$(rustc -vV | grep '^host:' | awk '{print $2}')"
readonly ASSET_NAME="fluent-${TARGET_TRIPLE}"

printf 'Building release binary for %s ...\n' "$TARGET_TRIPLE"
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml"

readonly BINARY="$REPO_ROOT/target/release/fluent"
if [[ ! -f "$BINARY" ]]; then
  printf 'error: release binary not found at %s\n' "$BINARY" >&2
  exit 1
fi

readonly STAGING="$REPO_ROOT/target/release-staging"
mkdir -p "$STAGING"
cp "$BINARY" "$STAGING/$ASSET_NAME"

printf 'Computing checksum ...\n'
CHECKSUM=$(shasum -a 256 "$STAGING/$ASSET_NAME" | awk '{print $1}')
printf '%s  %s\n' "$CHECKSUM" "$ASSET_NAME" > "$STAGING/${ASSET_NAME}.sha256"

printf 'Creating GitHub release %s ...\n' "$TAG"
gh release create "$TAG" \
  --title "$TAG" \
  --notes "Release ${version}" \
  "$STAGING/$ASSET_NAME" \
  "$STAGING/${ASSET_NAME}.sha256"

printf 'Released %s as %s\n' "$version" "$TAG"
printf '  asset:    %s\n' "$ASSET_NAME"
printf '  checksum: %s\n' "$CHECKSUM"
