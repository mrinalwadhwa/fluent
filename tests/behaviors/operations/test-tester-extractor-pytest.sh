#!/usr/bin/env bash
# test-tester-extractor-pytest — Verify the bundled extractor parses pytest -v.
#
# Drives extract-tester-results with a captured pytest harness command and
# asserts that each verbose result line becomes one normalized per-test result.
#
# Usage:
#   tests/behaviors/operations/test-tester-extractor-pytest.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

EXTRACTOR="${PROJECT_DIR}/.fluent/extract-tester-results"

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_parses_pytest_verbose_results() {
  WORK_DIR="$(mktemp -d -t pytest-extractor-XXXXXX)"
  ARTIFACT_DIR="${WORK_DIR}/artifacts"
  mkdir -p "$ARTIFACT_DIR"

  cat > "${ARTIFACT_DIR}/commands.json" <<'JSON'
[{"command": "python3 -m pytest -v", "test_harness": "pytest", "stdout_log": "commands/0-stdout.log"}]
JSON
  mkdir -p "${ARTIFACT_DIR}/commands"
  cat > "${ARTIFACT_DIR}/commands/0-stdout.log" <<'STDOUT'
============================= test session starts ==============================
collected 5 items

tests/test_feature.py::test_ok PASSED                                   [ 20%]
tests/test_feature.py::TestThing::test_fails FAILED                      [ 40%]
tests/test_feature.py::test_param[1-2] PASSED                              [ 60%]
tests/test_feature.py::test_skipped SKIPPED (not now)                    [ 80%]
tests/test_feature.py::test_expected_fail XFAIL                          [100%]

=========================== short test summary info ============================
FAILED tests/test_feature.py::TestThing::test_fails - assert 1 == 2
STDOUT

  local output result=0
  output="$(cd "$WORK_DIR" && python3 "$EXTRACTOR" "$ARTIFACT_DIR")" || result=1

  if ! printf '%s' "$output" | python3 -c '
import json, sys
tests = json.load(sys.stdin)
by_id = {test["id"]: test for test in tests}
expected = {
    "tests/test_feature.py::test_ok": "pass",
    "tests/test_feature.py::TestThing.test_fails": "fail",
    "tests/test_feature.py::test_param": "pass",
    "tests/test_feature.py::test_skipped": "skipped",
    "tests/test_feature.py::test_expected_fail": "skipped",
}
actual = {test_id: test["status"] for test_id, test in by_id.items()}
if actual != expected:
    print("unexpected pytest results:", json.dumps(tests, indent=2))
    sys.exit(1)
if any(test["test_harness"] != "pytest" for test in tests):
    print("every pytest result must retain the pytest harness")
    sys.exit(1)
'; then
    result=1
  fi

  rm -rf "$WORK_DIR"
  return $result
}

test_strips_ansi_and_ignores_progress_lines() {
  WORK_DIR="$(mktemp -d -t pytest-ansi-XXXXXX)"
  ARTIFACT_DIR="${WORK_DIR}/artifacts"
  mkdir -p "${ARTIFACT_DIR}/commands"

  cat > "${ARTIFACT_DIR}/commands.json" <<'JSON'
[{"command": "python3 -m pytest -v", "test_harness": "pytest", "stdout_log": "commands/0-stdout.log"}]
JSON
  printf '%s\n' \
    'collected 2 items' \
    $'\033[32mtests/test_ansi.py::test_pass\033[0m \033[32mPASSED\033[0m [ 50%]' \
    $'\033[31mtests/test_ansi.py::test_error\033[0m \033[31mERROR\033[0m [100%]' \
    > "${ARTIFACT_DIR}/commands/0-stdout.log"

  local output result=0
  output="$(cd "$WORK_DIR" && python3 "$EXTRACTOR" "$ARTIFACT_DIR")" || result=1

  if ! printf '%s' "$output" | python3 -c '
import json, sys
tests = json.load(sys.stdin)
by_id = {test["id"]: test["status"] for test in tests}
if by_id != {"tests/test_ansi.py::test_pass": "pass", "tests/test_ansi.py::test_error": "fail"}:
    print("ANSI-colored pytest lines must normalize:", by_id)
    sys.exit(1)
'; then
    result=1
  fi

  rm -rf "$WORK_DIR"
  return $result
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-tester-extractor-pytest\n\n'

run_test "parses pytest verbose results" test_parses_pytest_verbose_results
run_test "strips ANSI and ignores progress lines" test_strips_ansi_and_ignores_progress_lines

summarize_and_exit
