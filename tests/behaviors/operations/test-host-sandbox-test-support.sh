#!/usr/bin/env bash
# Verify the shell suite runs the copied test-support executable through the
# deterministic host-sandbox preflight seam.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

test_test_support_binary_consumes_host_preflight_control() {
  [ -x "${FLUENT_BIN_OVERRIDE:?shell harness must supply test-support Fluent}" ]
  FLUENT_TEST_HOST_SANDBOX_PREFLIGHT=fail \
    FLUENT_BIN_OVERRIDE="$FLUENT_BIN_OVERRIDE" \
    cargo nextest run --features test-support --test binary \
      test_support_binary_controls_host_sandbox_preflight
}

printf 'test-host-sandbox-test-support\n\n'

run_test "Copied test-support binary consumes host preflight control" \
  test_test_support_binary_consumes_host_preflight_control

summarize_and_exit
