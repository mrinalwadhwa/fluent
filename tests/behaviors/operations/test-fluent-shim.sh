#!/usr/bin/env bash
# test-fluent-shim — Verify the fluent shim content.
#
# Tests that the shim SKILL.md carries the expected bootstrap steps:
# binary install, fluent skills add, and hand-off to the full skill.
#
# Covers:
#   B2: shim carries the install command
#   B3: shim runs fluent skills add and hands off to the full skill
#
# Usage:
#   tests/behaviors/operations/test-fluent-shim.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

SHIM="$PROJECT_DIR/skills/fluent/SKILL.md"

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_shim_carries_install_command() {
  RESULT=0
  if ! grep -q 'fluent-shim: true' "$SHIM"; then
    printf '    FAIL: shim is missing the fluent-shim: true marker\n'
    RESULT=1
  fi
  if ! grep -q 'curl.*fluent.computer/install' "$SHIM"; then
    printf '    FAIL: shim does not carry the install command\n'
    RESULT=1
  fi
  return $RESULT
}

test_shim_runs_skills_add() {
  RESULT=0
  if ! grep -q 'fluent skills add' "$SHIM"; then
    printf '    FAIL: shim does not run fluent skills add\n'
    RESULT=1
  fi
  return $RESULT
}

test_shim_hands_off_to_data_directory() {
  RESULT=0
  if ! grep -q '\.local/share/fluent/skills/fluent/SKILL\.md' "$SHIM"; then
    printf '    FAIL: shim does not reference the data directory for hand-off\n'
    RESULT=1
  fi
  return $RESULT
}

test_shim_has_no_references() {
  RESULT=0
  if [ -d "$PROJECT_DIR/skills/fluent/references" ]; then
    printf '    FAIL: shim should not have a references/ directory\n'
    RESULT=1
  fi
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-fluent-shim\n\n'

run_test "shim carries the install command (B2)" test_shim_carries_install_command
run_test "shim runs fluent skills add (B3)" test_shim_runs_skills_add
run_test "shim hands off to data directory (B3)" test_shim_hands_off_to_data_directory
run_test "shim has no references" test_shim_has_no_references

summarize_and_exit
