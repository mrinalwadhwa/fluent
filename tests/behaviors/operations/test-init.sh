#!/usr/bin/env bash
# test-init — Verify factory init behavior.
#
# Tests the project initialization command from the user's perspective
# using only the factory binary's CLI interface.
#
# Covers:
#   - factory init creates .factory/ with project-level structure
#   - factory init reports already initialized when .factory/ exists
#
# Usage:
#   tests/behaviors/operations/test-init.sh

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

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_init_creates_factory_directory() {
  TEST_DIR="$(mktemp -d -t factory-test-init-XXXXXX)"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" init 2>&1)"

  RESULT=0
  if [ ! -d "${TEST_DIR}/.factory" ]; then
    printf '    FAIL: .factory/ directory was not created\n'
    RESULT=1
  fi
  if [ ! -d "${TEST_DIR}/.factory/runs" ]; then
    printf '    FAIL: .factory/runs/ directory was not created\n'
    RESULT=1
  fi
  if [ ! -d "${TEST_DIR}/.factory/expertise" ]; then
    printf '    FAIL: .factory/expertise/ directory was not created\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_init_reports_already_initialized() {
  TEST_DIR="$(mktemp -d -t factory-test-init-XXXXXX)"

  # First init
  cd "$TEST_DIR" && "$FACTORY_BIN" init > /dev/null 2>&1

  # Add a marker file to verify no changes are made
  echo "marker" > "${TEST_DIR}/.factory/expertise/marker.md"

  # Second init — should report already initialized
  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" init 2>&1)"

  RESULT=0
  if ! printf '%s' "$OUTPUT" | grep -qi "already"; then
    printf '    FAIL: expected "already initialized" message, got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  # Verify marker file is preserved (no changes made)
  if [ ! -f "${TEST_DIR}/.factory/expertise/marker.md" ]; then
    printf '    FAIL: existing files were removed\n'
    RESULT=1
  fi
  MARKER_CONTENT="$(cat "${TEST_DIR}/.factory/expertise/marker.md")"
  if [ "$MARKER_CONTENT" != "marker" ]; then
    printf '    FAIL: existing file content was changed\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-init\n\n'

run_test "init creates .factory/ with project structure" test_init_creates_factory_directory
run_test "init reports already initialized" test_init_reports_already_initialized

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
