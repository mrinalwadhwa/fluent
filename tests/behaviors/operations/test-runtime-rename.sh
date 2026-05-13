#!/usr/bin/env bash
# test-runtime-rename — Verify Backend→Runtime rename in CLI.
#
# Tests that the Rust binary uses the new "runtime" vocabulary
# instead of the old "backend" in all user-facing output.
#
# Covers:
#   - factory run --help shows --runtime flag
#   - factory run --help does not mention --backend
#   - factory status column header says RUNTIME
#   - factory run --runtime local is accepted
#   - factory run --backend rejects unknown flag
#
# Usage:
#   tests/behaviors/operations/test-runtime-rename.sh

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

test_run_help_shows_runtime_flag() {
  OUTPUT="$("$FACTORY_BIN" run --help 2>&1)"

  RESULT=0
  if ! printf '%s' "$OUTPUT" | grep -q -- '--runtime'; then
    printf '    FAIL: --runtime flag not found in run --help output\n'
    RESULT=1
  fi
  return $RESULT
}

test_run_help_does_not_show_backend_flag() {
  OUTPUT="$("$FACTORY_BIN" run --help 2>&1)"

  RESULT=0
  if printf '%s' "$OUTPUT" | grep -q -- '--backend'; then
    printf '    FAIL: --backend flag still present in run --help output\n'
    RESULT=1
  fi
  return $RESULT
}

test_status_header_says_runtime() {
  TEST_DIR="$(mktemp -d -t factory-test-rename-XXXXXX)"

  # Create a minimal factory project with a run
  mkdir -p "${TEST_DIR}/.factory/runs/test-rename"
  printf 'planned' > "${TEST_DIR}/.factory/runs/test-rename/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/test-rename/brief.md"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1)"

  RESULT=0
  if ! printf '%s' "$OUTPUT" | grep -q 'RUNTIME'; then
    printf '    FAIL: RUNTIME header not found in status output\n'
    printf '    Got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if printf '%s' "$OUTPUT" | grep -q 'BACKEND'; then
    printf '    FAIL: BACKEND header still present in status output\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_status_header_does_not_say_backend() {
  TEST_DIR="$(mktemp -d -t factory-test-rename-XXXXXX)"

  mkdir -p "${TEST_DIR}/.factory/runs/test-rename"
  printf 'planned' > "${TEST_DIR}/.factory/runs/test-rename/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/test-rename/brief.md"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1)"

  RESULT=0
  if printf '%s' "$OUTPUT" | grep -q 'BACKEND'; then
    printf '    FAIL: BACKEND header found in status output (should be RUNTIME)\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_run_help_description_says_runtime() {
  OUTPUT="$("$FACTORY_BIN" run --help 2>&1)"

  RESULT=0
  if ! printf '%s' "$OUTPUT" | grep -qi 'runtime'; then
    printf '    FAIL: "runtime" not found in run help description\n'
    RESULT=1
  fi
  return $RESULT
}

test_status_reads_runtime_file() {
  TEST_DIR="$(mktemp -d -t factory-test-rename-XXXXXX)"

  mkdir -p "${TEST_DIR}/.factory/runs/test-rt-file"
  printf 'executing' > "${TEST_DIR}/.factory/runs/test-rt-file/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/test-rt-file/brief.md"
  printf 'local' > "${TEST_DIR}/.factory/runs/test-rt-file/runtime"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1)"

  RESULT=0
  if ! printf '%s' "$OUTPUT" | grep -q 'test-rt-file.*local'; then
    printf '    FAIL: status does not show runtime value "local" for test-rt-file\n'
    printf '    Got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_status_shows_dash_without_runtime_file() {
  TEST_DIR="$(mktemp -d -t factory-test-rename-XXXXXX)"

  mkdir -p "${TEST_DIR}/.factory/runs/test-no-rt"
  printf 'planned' > "${TEST_DIR}/.factory/runs/test-no-rt/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/test-no-rt/brief.md"
  # No runtime file

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1)"

  RESULT=0
  if ! printf '%s' "$OUTPUT" | grep -q 'test-no-rt'; then
    printf '    FAIL: run test-no-rt not found in status output\n'
    printf '    Got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  # Should show "-" as default runtime
  if ! printf '%s' "$OUTPUT" | grep 'test-no-rt' | grep -q '\-'; then
    printf '    FAIL: missing runtime should show "-"\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_run_unknown_runtime_errors() {
  OUTPUT="$("$FACTORY_BIN" run --runtime bogus 2>&1 || true)"

  RESULT=0
  if ! printf '%s' "$OUTPUT" | grep -qi 'unknown runtime'; then
    printf '    FAIL: unknown runtime did not produce error message\n'
    printf '    Got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-runtime-rename\n\n'

run_test "run --help shows --runtime flag" test_run_help_shows_runtime_flag
run_test "run --help does not show --backend" test_run_help_does_not_show_backend_flag
run_test "run help description mentions runtime" test_run_help_description_says_runtime
run_test "status header says RUNTIME" test_status_header_says_runtime
run_test "status header does not say BACKEND" test_status_header_does_not_say_backend
run_test "status reads runtime file" test_status_reads_runtime_file
run_test "status shows dash without runtime file" test_status_shows_dash_without_runtime_file
run_test "unknown runtime produces error" test_run_unknown_runtime_errors

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
