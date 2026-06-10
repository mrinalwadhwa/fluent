#!/usr/bin/env bash
# test-watch-timeout — Verify watch --timeout and parent death detection behaviors.
#
# Tests:
#   - watch exits on timeout
#   - watch detects parent exit
#
# Usage:
#   tests/behaviors/operations/test-watch-timeout.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

PASS=0
FAIL=0
ERRORS=""

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

run_test() {
  TEST_NAME="$1"
  printf '  %s ... ' "$TEST_NAME"
  if ( eval "$2" ) 2>&1; then
    printf 'PASS\n'
    PASS=$((PASS + 1))
  else
    printf '\n'
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  - ${TEST_NAME}"
  fi
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_watch_exits_on_timeout() {
  TEST_DIR="$(mktemp -d -t factory-test-wt-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs"

  START="$(date +%s)"
  cd "$TEST_DIR" && timeout 10 "$FACTORY_BIN" watch --timeout 3 > /dev/null 2>&1
  EXIT_CODE=$?
  END="$(date +%s)"
  ELAPSED=$((END - START))

  rm -rf "$TEST_DIR"

  # Should exit (via timeout behavior) before the external timeout kills it.
  # We give it up to 8s — 3s timeout + grace period. If it ran the full 10s
  # it was killed by timeout(1) and the behavior did not fire.
  if [ "$ELAPSED" -ge 9 ]; then
    printf '    FAIL: watch ran for %ds; expected exit within 8s with --timeout 3\n' "$ELAPSED"
    return 1
  fi
  return 0
}

test_watch_detects_parent_exit() {
  TEST_DIR="$(mktemp -d -t factory-test-wpe-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs"
  PIDFILE="$(mktemp -t factory-wpe-pid-XXXXXX)"
  OUTFILE="$(mktemp -t factory-wpe-out-XXXXXX)"

  # Launch a child shell that starts watch, waits for it to initialize,
  # then exits — simulating a parent process death. The PID is written
  # to a file so we can track the specific process instead of using pgrep.
  (
    cd "$TEST_DIR"
    "$FACTORY_BIN" watch --timeout 15 > "$OUTFILE" 2>&1 &
    WATCH_PID=$!
    printf '%d' "$WATCH_PID" > "$PIDFILE"
    # Wait for watch to start and capture its parent PID before exiting
    sleep 2
    disown "$WATCH_PID"
    exit 0
  )

  WATCH_PID="$(cat "$PIDFILE")"

  # Give watch up to 8 seconds to detect parent exit and stop
  WAITED=0
  while [ "$WAITED" -lt 8 ]; do
    # Check if the specific process is still running
    if ! kill -0 "$WATCH_PID" 2>/dev/null; then
      rm -rf "$TEST_DIR" "$OUTFILE" "$PIDFILE"
      return 0
    fi
    sleep 1
    WAITED=$((WAITED + 1))
  done

  # Clean up the specific watch process
  kill "$WATCH_PID" 2>/dev/null || true

  rm -rf "$TEST_DIR" "$OUTFILE" "$PIDFILE"
  printf '    FAIL: watch did not exit after parent shell exited\n'
  return 1
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-watch-timeout\n\n'

run_test "watch exits on timeout" test_watch_exits_on_timeout
run_test "watch detects parent exit" test_watch_detects_parent_exit

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
