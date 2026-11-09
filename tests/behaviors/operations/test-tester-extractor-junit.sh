#!/usr/bin/env bash
# test-tester-extractor-junit — Verify the bundled extractor parses JUnit XML.
#
# Drives extract-tester-results with a captured junit harness command and
# asserts that each testcase becomes one normalized per-test result.
#
# Usage:
#   tests/behaviors/operations/test-tester-extractor-junit.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

EXTRACTOR="${PROJECT_DIR}/.fluent/extract-tester-results"

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_parses_junit_xml_cases() {
  WORK_DIR="$(mktemp -d -t junit-extractor-XXXXXX)"
  ARTIFACT_DIR="${WORK_DIR}/artifacts"
  mkdir -p "$ARTIFACT_DIR"

  cat > "${ARTIFACT_DIR}/results.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="pytest" tests="3" failures="1" skipped="1">
    <testcase classname="tests.test_feature" name="test_ok" file="tests/test_feature.py" time="0.125"/>
    <testcase classname="tests.test_feature.TestThing" name="test_fails" file="tests/test_feature.py" time="0.25">
      <failure message="boom">assert 1 == 2</failure>
    </testcase>
    <testcase classname="tests.test_feature" name="test_skipped" file="tests/test_feature.py">
      <skipped message="not now"/>
    </testcase>
  </testsuite>
</testsuites>
XML

  cat > "${ARTIFACT_DIR}/commands.json" <<JSON
[{"command": "python3 -m pytest --junitxml=${ARTIFACT_DIR}/results.xml", "test_harness": "junit"}]
JSON

  local output result=0
  output="$(cd "$WORK_DIR" && python3 "$EXTRACTOR" "$ARTIFACT_DIR")" || result=1

  if ! printf '%s' "$output" | python3 -c '
import json, sys
tests = json.load(sys.stdin)
by_id = {test["id"]: test for test in tests}
expected = {
    "tests/test_feature.py::test_ok": "pass",
    "tests/test_feature.py::TestThing.test_fails": "fail",
    "tests/test_feature.py::test_skipped": "skipped",
}
actual = {test_id: test["status"] for test_id, test in by_id.items()}
if actual != expected:
    print("unexpected JUnit results:", json.dumps(tests, indent=2))
    sys.exit(1)
if any(test["test_harness"] != "junit" for test in tests):
    print("every JUnit result must retain the junit harness")
    sys.exit(1)
if by_id["tests/test_feature.py::test_ok"]["duration_ms"] != 125:
    print("JUnit time must become milliseconds")
    sys.exit(1)
if by_id["tests/test_feature.py::TestThing.test_fails"]["failure_excerpt"] != "assert 1 == 2":
    print("the failure text must become the failure excerpt")
    sys.exit(1)
'; then
    result=1
  fi

  rm -rf "$WORK_DIR"
  return $result
}

test_resolves_a_relative_junit_path() {
  WORK_DIR="$(mktemp -d -t junit-relative-XXXXXX)"
  ARTIFACT_DIR="${WORK_DIR}/artifacts"
  mkdir -p "${WORK_DIR}/reports" "$ARTIFACT_DIR"

  cat > "${WORK_DIR}/reports/junit.xml" <<'XML'
<testsuites>
  <testsuite name="pytest" tests="1">
    <testcase classname="tests.test_relative" name="test_ok" file="tests/test_relative.py" time="0.01"/>
  </testsuite>
</testsuites>
XML

  cat > "${ARTIFACT_DIR}/commands.json" <<'JSON'
[{"command": "python3 -m pytest --junitxml=reports/junit.xml", "test_harness": "junit"}]
JSON

  local output result=0
  output="$(cd "$WORK_DIR" && python3 "$EXTRACTOR" "$ARTIFACT_DIR")" || result=1

  if ! printf '%s' "$output" | python3 -c '
import json, sys
tests = json.load(sys.stdin)
if [test["id"] for test in tests] != ["tests/test_relative.py::test_ok"]:
    print("the declared relative path must resolve from the candidate workspace:", tests)
    sys.exit(1)
'; then
    result=1
  fi

  rm -rf "$WORK_DIR"
  return $result
}

test_reports_a_missing_junit_file() {
  WORK_DIR="$(mktemp -d -t junit-missing-XXXXXX)"
  ARTIFACT_DIR="${WORK_DIR}/artifacts"
  mkdir -p "$ARTIFACT_DIR"

  cat > "${ARTIFACT_DIR}/commands.json" <<'JSON'
[{"command": "python3 -m pytest --junitxml=missing.xml", "test_harness": "junit"}]
JSON

  local output status=0 result=0
  output="$(cd "$WORK_DIR" && python3 "$EXTRACTOR" "$ARTIFACT_DIR" 2>&1)" || status=$?

  if [ "$status" -eq 0 ]; then
    printf '    FAIL: a missing JUnit result file must fail the extractor\n'
    result=1
  elif ! printf '%s' "$output" | grep -Fq "result file not found"; then
    printf '    FAIL: expected a missing-file diagnostic, got: %s\n' "$output"
    result=1
  fi

  rm -rf "$WORK_DIR"
  return $result
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-tester-extractor-junit\n\n'

run_test "parses JUnit XML test cases" test_parses_junit_xml_cases
run_test "resolves a relative JUnit path" test_resolves_a_relative_junit_path
run_test "reports a missing JUnit file" test_reports_a_missing_junit_file

summarize_and_exit
