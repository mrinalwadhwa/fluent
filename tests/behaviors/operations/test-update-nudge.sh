#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/fluent}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

TRIPLE="$(rustc -vV | grep '^host:' | awk '{print $2}')"
ASSET_NAME="fluent-${TRIPLE}"

setup_fixture() {
  local tmp="$1"
  local fixture="$tmp/fixture"
  local download="$fixture/download/v999.0.0"
  mkdir -p "$download"

  printf 'new-binary-content' > "$download/$ASSET_NAME"
  local sha
  sha=$(shasum -a 256 "$download/$ASSET_NAME" | awk '{print $1}')
  printf '%s  %s\n' "$sha" "$ASSET_NAME" > "$download/${ASSET_NAME}.sha256"

  local api_dir="$fixture/repos/test-owner/fluent/releases"
  mkdir -p "$api_dir"
  cat > "$api_dir/latest" <<JSON
{
  "tag_name": "v999.0.0",
  "assets": [
    {
      "name": "$ASSET_NAME",
      "browser_download_url": "file://$download/$ASSET_NAME"
    },
    {
      "name": "${ASSET_NAME}.sha256",
      "browser_download_url": "file://$download/${ASSET_NAME}.sha256"
    }
  ]
}
JSON
}

test_nudge_appears_on_stderr_when_behind() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  setup_fixture "$tmp"

  local stdout="$tmp/stdout.txt"
  local stderr="$tmp/stderr.txt"

  FLUENT_API_BASE="file://$tmp/fixture" \
  FLUENT_RELEASE_REPO="test-owner/fluent" \
  FLUENT_UPDATE_CACHE_PATH="$tmp/update-cache.json" \
    "$FLUENT_BIN" version > "$stdout" 2> "$stderr"

  if ! grep -q 'fluent update' "$stderr"; then
    printf '    FAIL: nudge should appear on stderr when behind\n'
    printf '    stderr was: %s\n' "$(cat "$stderr")"
    return 1
  fi

  if grep -q 'fluent update' "$stdout"; then
    printf '    FAIL: nudge must not appear on stdout\n'
    return 1
  fi
}

test_env_opt_out_suppresses_nudge() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  setup_fixture "$tmp"

  local stderr="$tmp/stderr.txt"

  FLUENT_API_BASE="file://$tmp/fixture" \
  FLUENT_RELEASE_REPO="test-owner/fluent" \
  FLUENT_UPDATE_CACHE_PATH="$tmp/update-cache.json" \
  FLUENT_NO_UPDATE_CHECK=1 \
    "$FLUENT_BIN" version > /dev/null 2> "$stderr"

  if grep -q 'fluent update' "$stderr"; then
    printf '    FAIL: FLUENT_NO_UPDATE_CHECK should suppress the nudge\n'
    printf '    stderr was: %s\n' "$(cat "$stderr")"
    return 1
  fi
}

printf 'test-update-nudge\n\n'

run_test "nudge appears on stderr when behind" test_nudge_appears_on_stderr_when_behind
run_test "FLUENT_NO_UPDATE_CHECK suppresses nudge" test_env_opt_out_suppresses_nudge

summarize_and_exit
