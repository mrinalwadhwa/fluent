#!/usr/bin/env bash
# test-behavior-bin-override - Verify operation scripts accept a Factory binary override.

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

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-bin-override-XXXXXX)"
  mkdir -p "$TEST_DIR/bin"
}

cleanup_test_project() {
  rm -rf "$TEST_DIR"
}

write_mock_factory() {
  cat > "$TEST_DIR/bin/factory" <<'MOCK_SCRIPT'
#!/usr/bin/env bash
printf '%s\n' "$0" >> "$FACTORY_OVERRIDE_LOG"
printf '%s\n' "$*" >> "$FACTORY_OVERRIDE_LOG"
exit 42
MOCK_SCRIPT
  chmod +x "$TEST_DIR/bin/factory"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -Fq -- "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

test_work_task_instructions_uses_factory_bin_override() {
  setup_test_project
  write_mock_factory

  RESULT=0
  if FACTORY_BIN_OVERRIDE="$TEST_DIR/bin/factory" \
      FACTORY_OVERRIDE_LOG="$TEST_DIR/factory.log" \
      bash "$PROJECT_DIR/tests/behaviors/operations/test-work-task-instructions.sh" \
        > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: updated script unexpectedly passed with sentinel mock\n'
    RESULT=1
  fi

  [ -f "$TEST_DIR/factory.log" ] || {
    printf '    FAIL: override binary was not invoked\n'
    RESULT=1
  }
  assert_contains "$(cat "$TEST_DIR/factory.log" 2>/dev/null || true)" "$TEST_DIR/bin/factory" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/factory.log" 2>/dev/null || true)" "work create" || RESULT=1

  cleanup_test_project
  return $RESULT
}

printf 'test-behavior-bin-override\n\n'

run_test "Work task instructions script uses FACTORY_BIN_OVERRIDE" \
  test_work_task_instructions_uses_factory_bin_override

printf '\n  %s passed, %s failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failed tests:%b\n' "$ERRORS"
  exit 1
fi
