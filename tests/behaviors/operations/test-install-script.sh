#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
INSTALL_SCRIPT="${PROJECT_DIR}/tools/install.sh"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)  ASSET_TRIPLE="aarch64-apple-darwin" ;;
  Darwin-x86_64) ASSET_TRIPLE="x86_64-apple-darwin" ;;
  Linux-x86_64)  ASSET_TRIPLE="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64) ASSET_TRIPLE="aarch64-unknown-linux-gnu" ;;
  *) ASSET_TRIPLE="unsupported" ;;
esac
ASSET_NAME="fluent-${ASSET_TRIPLE}"

setup_fixture_release() {
  local tmp="$1"
  local version="${2:-0.99.0}"
  local tag="v${version}"
  local releases_dir="$tmp/releases"
  local download_dir="$releases_dir/download/${tag}"
  mkdir -p "$download_dir"

  printf '#!/bin/sh\nprintf "fluent %s (abc1234)\\n"\n' "$version" \
    > "$download_dir/$ASSET_NAME"
  chmod +x "$download_dir/$ASSET_NAME"

  local latest_file="$releases_dir/latest"
  printf '%s\n' "$tag" > "$latest_file"
}

test_installs_runnable_binary() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  setup_fixture_release "$tmp" "0.99.0"

  local install_dir="$tmp/home/.local/bin"
  mkdir -p "$install_dir"

  FLUENT_INSTALL_BASE_URL="file://$tmp/releases" \
  HOME="$tmp/home" \
    bash "$INSTALL_SCRIPT" --install-path "$install_dir" --no-modify-path \
    > "$tmp/stdout.txt" 2>&1

  if [ ! -f "$install_dir/fluent" ]; then
    printf '    FAIL: binary not installed at %s\n' "$install_dir/fluent"
    return 1
  fi

  if [ ! -x "$install_dir/fluent" ]; then
    printf '    FAIL: installed binary is not executable\n'
    return 1
  fi

  local output
  output="$("$install_dir/fluent" 2>&1)" || true
  if ! printf '%s' "$output" | grep -q "fluent 0.99.0"; then
    printf '    FAIL: binary should run and report version; got: %s\n' "$output"
    return 1
  fi
}

printf 'test-install-script\n\n'

run_test "installs a runnable binary" test_installs_runnable_binary

summarize_and_exit
