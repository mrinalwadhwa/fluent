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

test_script_uses_factory_bin_override() {
  SCRIPT_PATH="$1"
  EXPECTED_ARGS="$2"

  setup_test_project
  write_mock_factory

  RESULT=0
  if FACTORY_BIN_OVERRIDE="$TEST_DIR/bin/factory" \
      FACTORY_OVERRIDE_LOG="$TEST_DIR/factory.log" \
      bash "$SCRIPT_PATH" \
        > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: updated script unexpectedly passed with sentinel mock\n'
    RESULT=1
  fi

  [ -f "$TEST_DIR/factory.log" ] || {
    printf '    FAIL: override binary was not invoked\n'
    RESULT=1
  }
  assert_contains "$(cat "$TEST_DIR/factory.log" 2>/dev/null || true)" "$TEST_DIR/bin/factory" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/factory.log" 2>/dev/null || true)" "$EXPECTED_ARGS" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_task_instructions_uses_factory_bin_override() {
  test_script_uses_factory_bin_override \
    "$PROJECT_DIR/tests/behaviors/operations/test-work-task-instructions.sh" \
    "work create"
}

test_work_task_run_uses_factory_bin_override() {
  test_script_uses_factory_bin_override \
    "$PROJECT_DIR/tests/behaviors/operations/test-work-task-run.sh" \
    "work create"
}

test_version_uses_factory_bin_override() {
  test_script_uses_factory_bin_override \
    "$PROJECT_DIR/tests/behaviors/operations/test-version.sh" \
    "version"
}

test_run_curation_uses_factory_bin_override() {
  test_script_uses_factory_bin_override \
    "$PROJECT_DIR/tests/behaviors/operations/test-run-curation.sh" \
    "status --runs"
}

test_operation_scripts_use_override_for_debug_binary() {
  RESULT=0
  UNSUPPORTED="$(
    grep -RIn 'target/debug/factory' "$PROJECT_DIR/tests/behaviors/operations" |
      grep -v '/test-behavior-bin-override.sh:' |
      grep -v 'FACTORY_BIN_OVERRIDE' || true
  )"

  if [ -n "$UNSUPPORTED" ]; then
    printf '    FAIL: scripts bind target/debug/factory without FACTORY_BIN_OVERRIDE\n'
    printf '%s\n' "$UNSUPPORTED"
    RESULT=1
  fi

  return $RESULT
}

test_operation_scripts_do_not_use_cargo_run_for_factory() {
  RESULT=0
  UNSUPPORTED="$(
    grep -RIn 'cargo run' "$PROJECT_DIR/tests/behaviors/operations" |
      grep -v '/test-behavior-bin-override.sh:' || true
  )"

  if [ -n "$UNSUPPORTED" ]; then
    printf '    FAIL: scripts invoke Factory through cargo run without FACTORY_BIN_OVERRIDE\n'
    printf '%s\n' "$UNSUPPORTED"
    RESULT=1
  fi

  return $RESULT
}

printf 'test-behavior-bin-override\n\n'

run_test "Work task instructions script uses FACTORY_BIN_OVERRIDE" \
  test_work_task_instructions_uses_factory_bin_override
run_test "Work task run script uses FACTORY_BIN_OVERRIDE" \
  test_work_task_run_uses_factory_bin_override
run_test "Version script uses FACTORY_BIN_OVERRIDE" \
  test_version_uses_factory_bin_override
run_test "Run curation script uses FACTORY_BIN_OVERRIDE" \
  test_run_curation_uses_factory_bin_override
run_test "Operation scripts use override for debug binary bindings" \
  test_operation_scripts_use_override_for_debug_binary
run_test "Operation scripts avoid cargo run Factory invocations" \
  test_operation_scripts_do_not_use_cargo_run_for_factory

printf '\n  %s passed, %s failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failed tests:%b\n' "$ERRORS"
  exit 1
fi
