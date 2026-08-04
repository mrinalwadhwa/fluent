#!/usr/bin/env bash
set -euo pipefail

if [[ "${TRACE-0}" == "1" ]]; then set -o xtrace; fi

command -v cargo >/dev/null 2>&1 || { printf 'error: cargo not found\n' >&2; exit 1; }
command -v gh >/dev/null 2>&1    || { printf 'error: gh CLI not found\n' >&2; exit 1; }
command -v codesign >/dev/null 2>&1 || { printf 'error: codesign not found\n' >&2; exit 1; }
command -v git >/dev/null 2>&1 || { printf 'error: git not found\n' >&2; exit 1; }
command -v shasum >/dev/null 2>&1 || { printf 'error: shasum not found\n' >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
readonly REPO_ROOT

version=$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')
if [[ -z "$version" ]]; then
  printf 'error: could not extract version from Cargo.toml\n' >&2
  exit 1
fi

readonly TAG="v${version}"
TARGET_TRIPLE="$(rustc -vV | grep '^host:' | awk '{print $2}')"
readonly TARGET_TRIPLE
readonly ASSET_NAME="fluent-${TARGET_TRIPLE}"
readonly CHECKSUM_NAME="${ASSET_NAME}.sha256"
readonly RELEASE_NOTES="$REPO_ROOT/documentation/releases/$TAG.md"

if [[ ! -f "$RELEASE_NOTES" ]]; then
  printf 'error: release notes not found at %s\n' "$RELEASE_NOTES" >&2
  exit 1
fi

if [[ -n "$(git -C "$REPO_ROOT" status --porcelain --untracked-files=all)" ]]; then
  printf 'error: release source tree is not clean\n' >&2
  exit 1
fi

printf 'Refreshing origin/main and release tags ...\n'
git -C "$REPO_ROOT" fetch --quiet origin main --tags

HEAD_COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD)"
readonly HEAD_COMMIT
ORIGIN_MAIN="$(git -C "$REPO_ROOT" rev-parse refs/remotes/origin/main)"
readonly ORIGIN_MAIN
if [[ "$HEAD_COMMIT" != "$ORIGIN_MAIN" ]]; then
  printf 'error: release commit %s does not equal origin/main %s\n' \
    "$HEAD_COMMIT" "$ORIGIN_MAIN" >&2
  exit 1
fi

if git -C "$REPO_ROOT" show-ref --verify --quiet "refs/tags/$TAG"; then
  printf 'error: release tag %s already exists\n' "$TAG" >&2
  exit 1
fi

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

# Apple Silicon refuses to execute an unsigned binary. Apply an ad-hoc
# signature (no Developer ID, no notarization, no secrets) so the published
# asset runs after a curl install or `fluent update` self-replace. Signing
# rewrites the binary, so it must happen before the checksum is computed.
printf 'Ad-hoc signing %s ...\n' "$ASSET_NAME"
codesign --sign - --force "$STAGING/$ASSET_NAME"
codesign --verify --strict "$STAGING/$ASSET_NAME"

digest="$(shasum -a 256 "$STAGING/$ASSET_NAME" | awk '{print $1}')"
printf '%s  %s\n' "$digest" "$ASSET_NAME" > "$STAGING/$CHECKSUM_NAME"

printf 'Running exact release gates ...\n'
cargo fmt --all --manifest-path "$REPO_ROOT/Cargo.toml" -- --check
cargo check --tests --features test-support --manifest-path "$REPO_ROOT/Cargo.toml"
"$BINARY" --no-sandbox tester check

if [[ -n "$(git -C "$REPO_ROOT" status --porcelain --untracked-files=all)" ]]; then
  printf 'error: release gates changed the source tree\n' >&2
  exit 1
fi

printf 'Creating GitHub release %s ...\n' "$TAG"
gh release create "$TAG" \
  --target "$HEAD_COMMIT" \
  --title "$TAG" \
  --notes-file "$RELEASE_NOTES" \
  "$STAGING/$ASSET_NAME" \
  "$STAGING/$CHECKSUM_NAME"

printf 'Released %s as %s\n' "$version" "$TAG"
printf '  asset: %s\n' "$ASSET_NAME"
printf '  checksum: %s\n' "$CHECKSUM_NAME"
