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
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

PASS=0
FAIL=0
ERRORS=""

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

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
