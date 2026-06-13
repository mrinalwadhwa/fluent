#!/usr/bin/env bash
# Shared test harness for shell behavior tests.
#
# Callers set LOG_DIR before sourcing, then invoke:
#   run_test "description" test_function_name
#
# At the end, call summarize_and_exit to print the failed-case summary
# and exit with the appropriate code.
#
# Honors FACTORY_TESTS_SKIP_LOG=1 to bypass per-case log-writing.

PASS=0
FAIL=0
ERRORS=""

run_test() {
  local case_label="$1"
  local case_fn="$2"

  printf '  %s ... ' "$case_label"

  if [ "${FACTORY_TESTS_SKIP_LOG:-0}" = "1" ] || [ -z "${LOG_DIR:-}" ]; then
    if ( eval "$case_fn" ) 2>&1; then
      printf 'PASS\n'
      PASS=$((PASS + 1))
    else
      printf '\n'
      FAIL=$((FAIL + 1))
      ERRORS="${ERRORS}\n  - ${case_label}"
    fi
    return
  fi

  local case_name="${case_fn#test_}"
  local log_file="${LOG_DIR}/${case_name}.log"
  mkdir -p "$LOG_DIR" 2>/dev/null || {
    if ( eval "$case_fn" ) 2>&1; then
      printf 'PASS\n'
      PASS=$((PASS + 1))
    else
      printf '\n'
      FAIL=$((FAIL + 1))
      ERRORS="${ERRORS}\n  - ${case_label}"
    fi
    return
  }

  local tmpfile
  tmpfile="$(mktemp 2>/dev/null)" || tmpfile=""
  if [ -z "$tmpfile" ]; then
    if ( eval "$case_fn" ) 2>&1; then
      printf 'PASS\n'
      PASS=$((PASS + 1))
    else
      printf '\n'
      FAIL=$((FAIL + 1))
      ERRORS="${ERRORS}\n  - ${case_label}"
    fi
    return
  fi

  local exit_code=0
  ( eval "$case_fn" ) > "$tmpfile" 2>&1 || exit_code=$?

  cat "$tmpfile" 2>/dev/null

  {
    printf '=== %s ===\n' "$case_label"
    printf 'function: %s\n' "$case_fn"
    printf -- '---output---\n'
    cat "$tmpfile" 2>/dev/null
  } > "$log_file" 2>/dev/null

  rm -f "$tmpfile"

  if [ "$exit_code" -eq 0 ]; then
    printf 'PASS\n'
    PASS=$((PASS + 1))
  else
    printf '\n'
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  - ${case_label}"

    local abs_log
    abs_log="$(cd "$(dirname "$log_file")" 2>/dev/null && pwd)/$(basename "$log_file")"
    printf '%s\n' "$abs_log" >> "${LOG_DIR}/.failed" 2>/dev/null
  fi
}

summarize_and_exit() {
  printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

  if [ "$FAIL" -gt 0 ]; then
    printf '\n  Failures:%b\n' "$ERRORS"
  fi

  if [ -s "${LOG_DIR:-.}/.failed" ]; then
    printf '\n  Failing case logs:\n'
    while IFS= read -r failed_log; do
      [ -n "$failed_log" ] || continue
      printf '    %s\n' "$failed_log"
      printf '    --- last 20 lines ---\n'
      tail -20 "$failed_log" 2>/dev/null | sed 's/^/      /'
    done < "${LOG_DIR}/.failed"
  fi

  if [ "$FAIL" -gt 0 ]; then
    exit 1
  fi
}
