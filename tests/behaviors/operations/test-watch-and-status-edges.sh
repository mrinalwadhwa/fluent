#!/usr/bin/env bash
# test-watch-and-status-edges — Verify watch and status edge cases via Rust binary.
#
# Tests observable behaviors:
#   - factory status displays fargate runtime
#   - factory status displays mixed runtimes
#   - factory watch polls at the specified interval
#   - factory watch polls at the default interval (60s)
#   - factory watch displays run status
#
# Usage:
#   tests/behaviors/operations/test-watch-and-status-edges.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

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

assert_output_contains() {
  if ! echo "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    return 1
  fi
}

# Run watch command, capture output to a temp file, kill after delay.
# Usage: run_watch <work_dir> <interval> <delay> <output_file>
run_watch() {
  local work_dir="$1" interval="$2" delay="$3" outfile="$4"
  cd "$work_dir" && "$FACTORY_BIN" watch "$interval" > "$outfile" 2>&1 &
  local pid=$!
  sleep "$delay"
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_status_displays_fargate_runtime() {
  TEST_DIR="$(mktemp -d -t factory-test-watch-XXXXXX)"

  mkdir -p "${TEST_DIR}/.factory/runs/run-fg"
  printf 'executing' > "${TEST_DIR}/.factory/runs/run-fg/status"
  printf 'Deploy to production' > "${TEST_DIR}/.factory/runs/run-fg/brief.md"
  printf 'fargate' > "${TEST_DIR}/.factory/runs/run-fg/runtime"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-fg" || RESULT=1
  assert_output_contains "$OUTPUT" "fargate" || RESULT=1
  assert_output_contains "$OUTPUT" "executing" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_status_displays_mixed_runtimes() {
  TEST_DIR="$(mktemp -d -t factory-test-watch-XXXXXX)"

  mkdir -p "${TEST_DIR}/.factory/runs/run-local" "${TEST_DIR}/.factory/runs/run-remote"
  printf 'executing' > "${TEST_DIR}/.factory/runs/run-local/status"
  printf 'Local run' > "${TEST_DIR}/.factory/runs/run-local/brief.md"
  printf 'local' > "${TEST_DIR}/.factory/runs/run-local/runtime"
  printf 'planned' > "${TEST_DIR}/.factory/runs/run-remote/status"
  printf 'Remote run' > "${TEST_DIR}/.factory/runs/run-remote/brief.md"
  printf 'fargate' > "${TEST_DIR}/.factory/runs/run-remote/runtime"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-local" || RESULT=1
  assert_output_contains "$OUTPUT" "local" || RESULT=1
  assert_output_contains "$OUTPUT" "run-remote" || RESULT=1
  assert_output_contains "$OUTPUT" "fargate" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_watch_reports_default_interval() {
  TEST_DIR="$(mktemp -d -t factory-test-watch-XXXXXX)"
  OUTFILE="$(mktemp -t factory-watch-out-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs"

  run_watch "$TEST_DIR" 60 2 "$OUTFILE"
  OUTPUT="$(cat "$OUTFILE")"

  RESULT=0
  assert_output_contains "$OUTPUT" "60s" || RESULT=1

  rm -rf "$TEST_DIR" "$OUTFILE"
  return $RESULT
}

test_watch_accepts_custom_interval() {
  TEST_DIR="$(mktemp -d -t factory-test-watch-XXXXXX)"
  OUTFILE="$(mktemp -t factory-watch-out-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs"

  run_watch "$TEST_DIR" 10 2 "$OUTFILE"
  OUTPUT="$(cat "$OUTFILE")"

  RESULT=0
  assert_output_contains "$OUTPUT" "10s" || RESULT=1

  rm -rf "$TEST_DIR" "$OUTFILE"
  return $RESULT
}

test_watch_displays_run_status() {
  TEST_DIR="$(mktemp -d -t factory-test-watch-XXXXXX)"
  OUTFILE="$(mktemp -t factory-watch-out-XXXXXX)"

  mkdir -p "${TEST_DIR}/.factory/runs/run-watched"
  printf 'executing' > "${TEST_DIR}/.factory/runs/run-watched/status"
  printf 'Watch target' > "${TEST_DIR}/.factory/runs/run-watched/brief.md"
  printf 'local' > "${TEST_DIR}/.factory/runs/run-watched/runtime"

  run_watch "$TEST_DIR" 2 3 "$OUTFILE"
  OUTPUT="$(cat "$OUTFILE")"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-watched" || RESULT=1
  assert_output_contains "$OUTPUT" "executing" || RESULT=1

  rm -rf "$TEST_DIR" "$OUTFILE"
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-watch-and-status-edges\n\n'

run_test "status displays fargate runtime" test_status_displays_fargate_runtime
run_test "status displays mixed runtimes" test_status_displays_mixed_runtimes
run_test "watch reports default interval" test_watch_reports_default_interval
run_test "watch accepts custom interval" test_watch_accepts_custom_interval
run_test "watch displays run status" test_watch_displays_run_status

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
