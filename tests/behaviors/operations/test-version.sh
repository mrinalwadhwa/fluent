#!/usr/bin/env bash
# test-version — Verify fluent version behavior.
#
# Tests the version command from the user's perspective using only the
# fluent binary's CLI interface.
#
# Covers:
#   - fluent version exits successfully outside a Fluent project
#   - fluent version prints the package version and build commit fallback
#
# Usage:
#   tests/behaviors/operations/test-version.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/fluent}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

test_version_without_fluent_project() {
  TEST_DIR="$(mktemp -d -t fluent-test-version-XXXXXX)"

  OUTPUT="$(cd "$TEST_DIR" && "$FLUENT_BIN" version 2>&1)"
  STATUS=$?

  RESULT=0
  if [ "$STATUS" -ne 0 ]; then
    printf '    FAIL: expected exit status 0, got %s\n' "$STATUS"
    RESULT=1
  fi
  if [ -d "${TEST_DIR}/.fluent" ]; then
    printf '    FAIL: command created .fluent/ in a non-project directory\n'
    RESULT=1
  fi
  if ! printf '%s' "$OUTPUT" | grep -Eq '[0-9]+\.[0-9]+\.[0-9]+'; then
    printf '    FAIL: output does not include a package version: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if ! printf '%s' "$OUTPUT" | grep -Eq ' ([0-9a-f]{7,40}|unknown)$'; then
    printf '    FAIL: output does not include a build commit or fallback: %s\n' "$OUTPUT"
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

printf 'test-version\n\n'

run_test "version prints package version and build metadata outside a Fluent project" test_version_without_fluent_project

summarize_and_exit
