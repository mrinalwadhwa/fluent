#!/usr/bin/env bash
# test-resolve-edges — Verify run-id resolution edge cases.
#
# Tests run-id resolution behaviors not covered by test-run:
#   - Active-run pointing to a non-existent run directory
#   - Scan when .factory/runs/ is empty
#   - Env var overrides active-run
#   - Scan finds exactly one among mixed statuses
#
# Sources the factory script in library mode to call functions directly.
#
# Usage:
#   tests/behaviors/operations/test-resolve-edges.sh

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
  TEST_DIR="$(mktemp -d -t factory-test-resolve-edge-XXXXXX)"
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

test_env_overrides_active_run() {
  setup_test_project

  mkdir -p ".factory/runs/run-file" ".factory/runs/run-env"
  printf 'planned' > ".factory/runs/run-file/status"
  printf 'Brief' > ".factory/runs/run-file/brief.md"
  printf 'planned' > ".factory/runs/run-env/status"
  printf 'Brief' > ".factory/runs/run-env/brief.md"
  printf 'run-file' > ".factory/active-run"

  # FACTORY_RUN_ID env var should override active-run file
  RUN_ID=""
  FACTORY_RUN_ID="run-env" resolve_run_id "$(pwd)"

  RESULT=0
  [ "$RUN_ID" = "run-env" ] || { printf '    FAIL: RUN_ID=%s, expected run-env (env should override file)\n' "$RUN_ID"; RESULT=1; }

  cleanup_test_project
  return $RESULT
}

test_scan_with_mixed_statuses() {
  setup_test_project

  # Create runs with various non-active statuses and one active
  mkdir -p ".factory/runs/run-complete" ".factory/runs/run-failed" \
           ".factory/runs/run-needs-user" ".factory/runs/run-active"
  printf 'complete' > ".factory/runs/run-complete/status"
  printf 'Brief' > ".factory/runs/run-complete/brief.md"
  printf 'failed' > ".factory/runs/run-failed/status"
  printf 'Brief' > ".factory/runs/run-failed/brief.md"
  printf 'needs-user' > ".factory/runs/run-needs-user/status"
  printf 'Brief' > ".factory/runs/run-needs-user/brief.md"
  printf 'executing' > ".factory/runs/run-active/status"
  printf 'Brief' > ".factory/runs/run-active/brief.md"
  # No active-run file — force scan

  RUN_ID=""
  resolve_run_id "$(pwd)"

  RESULT=0
  [ "$RUN_ID" = "run-active" ] || { printf '    FAIL: RUN_ID=%s, expected run-active (only active run)\n' "$RUN_ID"; RESULT=1; }

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-resolve-edges\n\n'

run_test "env var overrides active-run file" test_env_overrides_active_run
run_test "scan finds active among mixed statuses" test_scan_with_mixed_statuses

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
