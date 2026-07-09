#!/usr/bin/env bash
# test-behavior-bin-override - Verify operation scripts accept a Fluent binary override.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_test_project() {
  TEST_DIR="$(mktemp -d -t fluent-bin-override-XXXXXX)"
  mkdir -p "$TEST_DIR/bin"
}

cleanup_test_project() {
  rm -rf "$TEST_DIR"
}

write_mock_fluent() {
  cat > "$TEST_DIR/bin/fluent" <<'MOCK_SCRIPT'
#!/usr/bin/env bash
printf '%s\n' "$0" >> "$FLUENT_OVERRIDE_LOG"
printf '%s\n' "$*" >> "$FLUENT_OVERRIDE_LOG"
exit 42
MOCK_SCRIPT
  chmod +x "$TEST_DIR/bin/fluent"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -Fq -- "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

test_script_uses_fluent_bin_override() {
  SCRIPT_PATH="$1"
  EXPECTED_ARGS="$2"

  setup_test_project
  write_mock_fluent

  RESULT=0
  if FLUENT_BIN_OVERRIDE="$TEST_DIR/bin/fluent" \
      FLUENT_OVERRIDE_LOG="$TEST_DIR/fluent.log" \
      bash "$SCRIPT_PATH" \
        > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: updated script unexpectedly passed with sentinel mock\n'
    RESULT=1
  fi

  [ -f "$TEST_DIR/fluent.log" ] || {
    printf '    FAIL: override binary was not invoked\n'
    RESULT=1
  }
  assert_contains "$(cat "$TEST_DIR/fluent.log" 2>/dev/null || true)" "$TEST_DIR/bin/fluent" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/fluent.log" 2>/dev/null || true)" "$EXPECTED_ARGS" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_task_instructions_uses_fluent_bin_override() {
  test_script_uses_fluent_bin_override \
    "$PROJECT_DIR/tests/behaviors/operations/test-work-task-instructions.sh" \
    "work-item create"
}

test_work_task_run_uses_fluent_bin_override() {
  test_script_uses_fluent_bin_override \
    "$PROJECT_DIR/tests/behaviors/operations/test-work-task-run.sh" \
    "work-item create"
}

test_version_uses_fluent_bin_override() {
  test_script_uses_fluent_bin_override \
    "$PROJECT_DIR/tests/behaviors/operations/test-version.sh" \
    "version"
}

test_operation_scripts_use_override_for_debug_binary() {
  RESULT=0
  UNSUPPORTED="$(
    grep -RIn 'target/debug/fluent' "$PROJECT_DIR/tests/behaviors/operations" |
      grep -v '/test-behavior-bin-override.sh:' |
      grep -v 'FLUENT_BIN_OVERRIDE' || true
  )"

  if [ -n "$UNSUPPORTED" ]; then
    printf '    FAIL: scripts bind target/debug/fluent without FLUENT_BIN_OVERRIDE\n'
    printf '%s\n' "$UNSUPPORTED"
    RESULT=1
  fi

  return $RESULT
}

test_operation_scripts_do_not_use_cargo_run_for_fluent() {
  RESULT=0
  UNSUPPORTED="$(
    grep -RIn 'cargo run' "$PROJECT_DIR/tests/behaviors/operations" |
      grep -v '/test-behavior-bin-override.sh:' || true
  )"

  if [ -n "$UNSUPPORTED" ]; then
    printf '    FAIL: scripts invoke Fluent through cargo run without FLUENT_BIN_OVERRIDE\n'
    printf '%s\n' "$UNSUPPORTED"
    RESULT=1
  fi

  return $RESULT
}

printf 'test-behavior-bin-override\n\n'

run_test "Work task instructions script uses FLUENT_BIN_OVERRIDE" \
  test_work_task_instructions_uses_fluent_bin_override
run_test "Work task run script uses FLUENT_BIN_OVERRIDE" \
  test_work_task_run_uses_fluent_bin_override
run_test "Version script uses FLUENT_BIN_OVERRIDE" \
  test_version_uses_fluent_bin_override
run_test "Operation scripts use override for debug binary bindings" \
  test_operation_scripts_use_override_for_debug_binary
run_test "Operation scripts avoid cargo run Fluent invocations" \
  test_operation_scripts_do_not_use_cargo_run_for_fluent

summarize_and_exit
