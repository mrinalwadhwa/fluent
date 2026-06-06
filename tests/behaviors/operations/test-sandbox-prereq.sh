#!/usr/bin/env bash
# test-sandbox-prereq - Verify the sandbox suite requires Seatbelt.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

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

assert_output_contains() {
  if ! printf '%s' "$1" | grep -Fq "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    return 1
  fi
}

test_sandbox_suite_requires_working_seatbelt_runtime() {
  TEST_DIR="$(mktemp -d -t factory-test-sandbox-prereq-XXXXXX)"
  MOCK_BIN="${TEST_DIR}/bin"
  mkdir -p "$MOCK_BIN"

  cat > "${MOCK_BIN}/sandbox-exec" << 'MOCK_SCRIPT'
#!/usr/bin/env bash
if [ "${1:-}" = "-p" ]; then
  exit 71
fi
exec /usr/bin/false
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/sandbox-exec"

  set +e
  OUTPUT="$(PATH="${MOCK_BIN}:${PATH}" "${PROJECT_DIR}/tests/behaviors/operations/test-sandbox.sh" 2>&1)"
  STATUS=$?
  set -e

  RESULT=0
  if [ "$STATUS" -eq 0 ]; then
    printf '    FAIL: sandbox suite succeeded without a working Seatbelt runtime\n'
    RESULT=1
  fi

  assert_output_contains "$OUTPUT" "sandbox-exec cannot apply profiles" || RESULT=1
  assert_output_contains "$OUTPUT" "Sandbox behavior coverage requires a working Seatbelt runtime." || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

printf 'test-sandbox-prereq\n\n'

run_test "sandbox suite requires working Seatbelt runtime" test_sandbox_suite_requires_working_seatbelt_runtime

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -ne 0 ]; then
  printf '\nFailed tests:%b\n' "$ERRORS"
  exit 1
fi
