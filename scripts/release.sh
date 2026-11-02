#!/usr/bin/env bash
set -euo pipefail

if [[ "${TRACE-0}" == "1" ]]; then set -o xtrace; fi

if [[ $# -ne 2 ]]; then
  printf 'usage: %s <work-item-id> <attempt-id>\n' "$0" >&2
  exit 2
fi
for identity in "$@"; do
  if [[ ! "$identity" =~ ^[a-zA-Z0-9][a-zA-Z0-9._-]*$ || "$identity" == . || "$identity" == .. ]]; then
    printf 'usage: %s <work-item-id> <attempt-id>\n' "$0" >&2
    exit 2
  fi
done
readonly RELEASE_WORK="$1"
readonly RELEASE_ATTEMPT="$2"

command -v cargo >/dev/null 2>&1 || { printf 'error: cargo not found\n' >&2; exit 1; }
command -v gh >/dev/null 2>&1    || { printf 'error: gh CLI not found\n' >&2; exit 1; }
command -v codesign >/dev/null 2>&1 || { printf 'error: codesign not found\n' >&2; exit 1; }
command -v git >/dev/null 2>&1 || { printf 'error: git not found\n' >&2; exit 1; }
command -v lipo >/dev/null 2>&1 || { printf 'error: lipo not found\n' >&2; exit 1; }
command -v rustc >/dev/null 2>&1 || { printf 'error: rustc not found\n' >&2; exit 1; }
command -v shasum >/dev/null 2>&1 || { printf 'error: shasum not found\n' >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
readonly REPO_ROOT

version=$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')
if [[ -z "$version" ]]; then
  printf 'error: could not extract version from Cargo.toml\n' >&2
  exit 1
fi

readonly TAG="v${version}"
HOST_TARGET="$(rustc -vV | grep '^host:' | awk '{print $2}')"
readonly HOST_TARGET
readonly -a RELEASE_TARGETS=(
  "aarch64-apple-darwin"
  "x86_64-apple-darwin"
)
readonly RELEASE_NOTES="$REPO_ROOT/documentation/releases/$TAG.md"

case "$HOST_TARGET" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *)
    printf 'error: releases must run on a supported macOS host, found %s\n' \
      "$HOST_TARGET" >&2
    exit 1
    ;;
esac

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

readonly STAGING="$REPO_ROOT/target/release-staging"
mkdir -p "$STAGING"
cp "$REPO_ROOT/tools/ghostty-LICENSE" "$STAGING/ghostty-LICENSE"
declare -a RELEASE_ASSETS=("$STAGING/ghostty-LICENSE")

for target in "${RELEASE_TARGETS[@]}"; do
  asset_name="fluent-${target}"
  checksum_name="${asset_name}.sha256"
  binary="$REPO_ROOT/target/$target/release/fluent"
  staged_binary="$STAGING/$asset_name"

  printf 'Building release binary for %s ...\n' "$target"
  bash "$REPO_ROOT/scripts/prepare-terminal-deps.sh" "$target"
  cargo build --release --target "$target" \
    --manifest-path "$REPO_ROOT/Cargo.toml"

  if [[ ! -f "$binary" ]]; then
    printf 'error: release binary not found at %s\n' "$binary" >&2
    exit 1
  fi
  cp "$binary" "$staged_binary"

  case "$target" in
    aarch64-apple-darwin) expected_arch="arm64" ;;
    x86_64-apple-darwin) expected_arch="x86_64" ;;
  esac
  actual_arch="$(lipo -archs "$staged_binary")"
  if [[ "$actual_arch" != "$expected_arch" ]]; then
    printf 'error: %s contains architecture %s, expected %s\n' \
      "$asset_name" "$actual_arch" "$expected_arch" >&2
    exit 1
  fi

  # Apply an ad-hoc signature before computing the checksum because signing
  # rewrites the binary. The release does not use Developer ID credentials.
  printf 'Ad-hoc signing %s ...\n' "$asset_name"
  codesign --sign - --force "$staged_binary"
  codesign --verify --strict "$staged_binary"

  digest="$(shasum -a 256 "$staged_binary" | awk '{print $1}')"
  printf '%s  %s\n' "$digest" "$asset_name" > "$STAGING/$checksum_name"
  RELEASE_ASSETS+=("$staged_binary" "$STAGING/$checksum_name")
done

# tester verify also requires the candidate's reviewed release-host contract.
printf 'Running exact release gates and host proof verification ...\n'
cargo fmt --all --manifest-path "$REPO_ROOT/Cargo.toml" -- --check
cargo check --tests --features test-support --manifest-path "$REPO_ROOT/Cargo.toml"
(
  cd "$REPO_ROOT"
  "$REPO_ROOT/target/$HOST_TARGET/release/fluent" tester verify \
    "$RELEASE_WORK" "$RELEASE_ATTEMPT" --candidate "$HEAD_COMMIT"
)

if [[ -n "$(git -C "$REPO_ROOT" status --porcelain --untracked-files=all)" ]]; then
  printf 'error: release gates changed the source tree\n' >&2
  exit 1
fi

printf 'Creating GitHub release %s ...\n' "$TAG"
gh release create "$TAG" \
  --target "$HEAD_COMMIT" \
  --title "$TAG" \
  --notes-file "$RELEASE_NOTES" \
  "${RELEASE_ASSETS[@]}"

printf 'Released %s as %s\n' "$version" "$TAG"
for target in "${RELEASE_TARGETS[@]}"; do
  printf '  asset: fluent-%s\n' "$target"
  printf '  checksum: fluent-%s.sha256\n' "$target"
done
