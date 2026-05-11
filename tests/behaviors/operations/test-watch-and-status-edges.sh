#!/usr/bin/env bash
# test-watch-and-status-edges — Verify watch and status edge-case behaviors.
#
# Tests observable behaviors from documentation/behaviors.md that involve
# factory watch and factory status with different backends.
#
# Sources the factory script in library mode to call functions directly.
#
# Covers:
#   - factory status displays fargate backend correctly
#   - factory watch polls at the specified interval
#   - factory watch polls at the default interval (60s)
#
# Usage:
#   tests/behaviors/operations/test-watch-and-status-edges.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY="${PROJECT_DIR}/scripts/factory"

PASS=0
FAIL=0
ERRORS=""

# Source factory functions (library mode — no dispatch)
FACTORY_LIB=1 . "$FACTORY"

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-watch-XXXXXX)"
  mkdir -p "${TEST_DIR}/main"
  cd "${TEST_DIR}/main"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  echo "test" > README.md
  git add . && git commit -m "init" > /dev/null 2>&1
}

cleanup_test_project() {
  cd /
  rm -rf "$TEST_DIR"
}

assert_output_contains() {
  if ! echo "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    return 1
  fi
}

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

test_status_displays_fargate_backend() {
  setup_test_project

  mkdir -p ".factory/runs/run-fg"
  printf 'executing' > ".factory/runs/run-fg/status"
  printf 'Deploy to production' > ".factory/runs/run-fg/brief.md"
  printf 'fargate' > ".factory/runs/run-fg/backend"

  OUTPUT="$(cmd_status "$(pwd)" 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-fg" || RESULT=1
  assert_output_contains "$OUTPUT" "fargate" || RESULT=1
  assert_output_contains "$OUTPUT" "executing" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_status_displays_mixed_backends() {
  setup_test_project

  mkdir -p ".factory/runs/run-local" ".factory/runs/run-remote"
  printf 'executing' > ".factory/runs/run-local/status"
  printf 'Local run' > ".factory/runs/run-local/brief.md"
  printf 'local' > ".factory/runs/run-local/backend"
  printf 'planned' > ".factory/runs/run-remote/status"
  printf 'Remote run' > ".factory/runs/run-remote/brief.md"
  printf 'fargate' > ".factory/runs/run-remote/backend"

  OUTPUT="$(cmd_status "$(pwd)" 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-local" || RESULT=1
  assert_output_contains "$OUTPUT" "local" || RESULT=1
  assert_output_contains "$OUTPUT" "run-remote" || RESULT=1
  assert_output_contains "$OUTPUT" "fargate" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_watch_reports_default_interval() {
  setup_test_project
  mkdir -p ".factory/runs"

  # Run watch in background, capture first few lines, then kill
  OUTPUT="$("$FACTORY" watch 2>&1 & PID=$!; sleep 2; kill $PID 2>/dev/null; wait $PID 2>/dev/null; true)"

  RESULT=0
  # Watch should report its polling interval
  assert_output_contains "$OUTPUT" "60s" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_watch_accepts_custom_interval() {
  setup_test_project
  mkdir -p ".factory/runs"

  OUTPUT="$("$FACTORY" watch 10 2>&1 & PID=$!; sleep 2; kill $PID 2>/dev/null; wait $PID 2>/dev/null; true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "10s" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_watch_displays_run_status() {
  setup_test_project

  mkdir -p ".factory/runs/run-watched"
  printf 'executing' > ".factory/runs/run-watched/status"
  printf 'Watch target' > ".factory/runs/run-watched/brief.md"
  printf 'local' > ".factory/runs/run-watched/backend"

  OUTPUT="$("$FACTORY" watch 2 2>&1 & PID=$!; sleep 3; kill $PID 2>/dev/null; wait $PID 2>/dev/null; true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-watched" || RESULT=1
  assert_output_contains "$OUTPUT" "executing" || RESULT=1

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-watch-and-status-edges\n\n'

run_test "status displays fargate backend" test_status_displays_fargate_backend
run_test "status displays mixed backends" test_status_displays_mixed_backends
run_test "watch reports default interval" test_watch_reports_default_interval
run_test "watch accepts custom interval" test_watch_accepts_custom_interval
run_test "watch displays run status" test_watch_displays_run_status

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
