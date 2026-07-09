#!/usr/bin/env bash
# test-tester-integration-coverage — Verify the Tester reports integration tests.
#
# Drives the extract-tester-results script with captured nextest
# libtest-json output containing an integration test, and asserts the
# test appears with its status in the extracted results.
#
# Covers:
#   - B1: integration tests from tests/binary.rs appear in tester results
#
# Usage:
#   tests/behaviors/operations/test-tester-integration-coverage.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

EXTRACTOR="${PROJECT_DIR}/.fluent/extract-tester-results"

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_integration_test_appears_in_extracted_results() {
  ARTIFACT_DIR="$(mktemp -d -t tester-artifacts-XXXXXX)"

  cat > "${ARTIFACT_DIR}/commands.json" <<'CMDJSON'
[{"test_harness": "cargo-nextest", "stdout_log": "nextest-stdout.log"}]
CMDJSON

  cat > "${ARTIFACT_DIR}/nextest-stdout.log" <<'NEXTEST'
{"type":"suite","event":"started","test_count":2}
{"type":"test","event":"started","name":"fluent::binary$init_writes_gitignore_when_absent"}
{"type":"test","event":"ok","name":"fluent::binary$init_writes_gitignore_when_absent","exec_time":0.359877709}
{"type":"suite","event":"ok","passed":1,"failed":0,"ignored":1,"measured":0,"filtered_out":164,"exec_time":0.359877709}
NEXTEST

  OUTPUT="$(python3 "$EXTRACTOR" "$ARTIFACT_DIR")"

  RESULT=0

  if ! printf '%s' "$OUTPUT" | python3 -c "
import json, sys
tests = json.load(sys.stdin)
matches = [t for t in tests if t['id'] == 'fluent::binary\$init_writes_gitignore_when_absent']
if not matches:
    print('FAIL: integration test not found in results')
    sys.exit(1)
if matches[0]['status'] != 'pass':
    print('FAIL: expected status pass, got ' + matches[0]['status'])
    sys.exit(1)
if matches[0]['test_harness'] != 'cargo-nextest':
    print('FAIL: expected harness cargo-nextest, got ' + matches[0]['test_harness'])
    sys.exit(1)
"; then
    RESULT=1
  fi

  rm -rf "$ARTIFACT_DIR"
  return $RESULT
}

test_tester_yaml_nextest_command_has_env_var() {
  TESTER_YAML="${PROJECT_DIR}/.fluent/tester.yaml"

  RESULT=0

  if ! grep -q 'NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1' "$TESTER_YAML"; then
    printf '    FAIL: tester.yaml nextest command missing NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1\n'
    RESULT=1
  fi

  if grep -q 'cargo test --lib' "$TESTER_YAML"; then
    printf '    FAIL: tester.yaml still contains the broken cargo test --lib entry\n'
    RESULT=1
  fi

  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-tester-integration-coverage\n\n'

run_test "integration test appears in extracted results" test_integration_test_appears_in_extracted_results
run_test "tester.yaml nextest command has env var and no cargo test --lib" test_tester_yaml_nextest_command_has_env_var

summarize_and_exit
