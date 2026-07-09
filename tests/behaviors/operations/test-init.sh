#!/usr/bin/env bash
# test-init — Verify fluent init behavior.
#
# Tests the project initialization command from the user's perspective
# using only the fluent binary's CLI interface.
#
# Covers:
#   - fluent init creates .fluent/ with project-level structure
#   - fluent init reports already initialized when .fluent/ exists
#
# Usage:
#   tests/behaviors/operations/test-init.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/fluent}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_init_creates_fluent_directory() {
  TEST_DIR="$(mktemp -d -t fluent-test-init-XXXXXX)"

  OUTPUT="$(cd "$TEST_DIR" && "$FLUENT_BIN" init 2>&1)"

  RESULT=0
  if [ ! -d "${TEST_DIR}/.fluent" ]; then
    printf '    FAIL: .fluent/ directory was not created\n'
    RESULT=1
  fi
  if [ ! -d "${TEST_DIR}/.fluent/expertise" ]; then
    printf '    FAIL: .fluent/expertise/ directory was not created\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_init_reports_already_initialized() {
  TEST_DIR="$(mktemp -d -t fluent-test-init-XXXXXX)"

  # First init
  cd "$TEST_DIR" && "$FLUENT_BIN" init > /dev/null 2>&1

  # Add a marker file to verify no changes are made
  echo "marker" > "${TEST_DIR}/.fluent/expertise/marker.md"

  # Second init — should report already initialized
  OUTPUT="$(cd "$TEST_DIR" && "$FLUENT_BIN" init 2>&1)"

  RESULT=0
  if ! printf '%s' "$OUTPUT" | grep -qi "already"; then
    printf '    FAIL: expected "already initialized" message, got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  # Verify marker file is preserved (no changes made)
  if [ ! -f "${TEST_DIR}/.fluent/expertise/marker.md" ]; then
    printf '    FAIL: existing files were removed\n'
    RESULT=1
  fi
  MARKER_CONTENT="$(cat "${TEST_DIR}/.fluent/expertise/marker.md")"
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

run_test "init creates .fluent/ with project structure" test_init_creates_fluent_directory
run_test "init reports already initialized" test_init_reports_already_initialized

summarize_and_exit
