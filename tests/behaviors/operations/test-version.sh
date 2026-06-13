#!/usr/bin/env bash
# test-version — Verify factory version behavior.
#
# Tests the version command from the user's perspective using only the
# factory binary's CLI interface.
#
# Covers:
#   - factory version exits successfully outside a Factory project
#   - factory version prints the package version and build commit fallback
#
# Usage:
#   tests/behaviors/operations/test-version.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

test_version_without_factory_project() {
  TEST_DIR="$(mktemp -d -t factory-test-version-XXXXXX)"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" version 2>&1)"
  STATUS=$?

  RESULT=0
  if [ "$STATUS" -ne 0 ]; then
    printf '    FAIL: expected exit status 0, got %s\n' "$STATUS"
    RESULT=1
  fi
  if [ -d "${TEST_DIR}/.factory" ]; then
    printf '    FAIL: command created .factory/ in a non-project directory\n'
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

run_test "version prints package version and build metadata outside a Factory project" test_version_without_factory_project

summarize_and_exit
