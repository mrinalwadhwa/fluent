#!/usr/bin/env bash
# Keep real tmux coverage mandatory without granting shared sockets to workers.
set -euo pipefail
: "${FLUENT_TEST_REAL_TERMINAL_HOST:?host terminal authorization is required}"
test "$FLUENT_TEST_REAL_TERMINAL_HOST" = 1
test "$(uname -s)" = Darwin
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
unset FLUENT_TEST_HOST_SANDBOX_PREFLIGHT
unset FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC
unset FLUENT_TEST_TERMINAL_CLIENT
export NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1
if [[ -n "${FLUENT_DASHBOARD_HOST_OUTPUT_DIR:-}" ]]; then
  mkdir "$FLUENT_DASHBOARD_HOST_OUTPUT_DIR"
  mkdir "$FLUENT_DASHBOARD_HOST_OUTPUT_DIR/pty"
  export FLUENT_DASHBOARD_PTY_EVIDENCE_DIR="$FLUENT_DASHBOARD_HOST_OUTPUT_DIR/pty"
fi
cargo nextest run --offline --features test-support --lib --test dashboard_terminal \
  --no-tests fail --no-fail-fast --run-ignored all --test-threads 4 --message-format libtest-json \
  -E "$(cat tests/fixtures/dashboard-terminal-host.filter)"
