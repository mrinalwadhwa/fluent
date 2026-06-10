#!/usr/bin/env bash
# test-notification-content — Verify notification content for watch status changes.
#
# Tests observable behaviors:
#   - notification includes run ID and status
#   - notification includes brief summary
#   - complete notification includes review verdict
#   - complete notification includes session count
#   - needs-user notification includes handoff content
#   - failed notification includes run ID, status, and brief
#
# Approach: intercepts osascript via PATH override to capture notification
# text without actually sending macOS notifications.
#
# Usage:
#   tests/behaviors/operations/test-notification-content.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

PASS=0
FAIL=0
ERRORS=""

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

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

assert_notif_contains() {
  if ! grep -q "$2" "$1"; then
    printf '    FAIL: notification does not contain "%s"\n' "$2"
    printf '    Notification content:\n'
    sed 's/^/      /' "$1"
    return 1
  fi
}

# Create a fake osascript that logs all arguments to NOTIF_LOG.
setup_fake_osascript() {
  local dir="$1"
  local bin_dir="${dir}/bin"
  mkdir -p "$bin_dir"
  cat > "${bin_dir}/osascript" << 'FAKESCRIPT'
#!/bin/sh
for arg in "$@"; do
  printf -- '%s\n' "$arg" >> "$NOTIF_LOG"
done
printf -- '---\n' >> "$NOTIF_LOG"
FAKESCRIPT
  chmod +x "${bin_dir}/osascript"
  echo "$bin_dir"
}

# Run watch, trigger a status change, capture notification.
watch_with_status_change() {
  local test_dir="$1" fake_bin_dir="$2" run_id="$3" new_status="$4" notif_log="$5"
  local outfile
  outfile="$(mktemp -t factory-watch-out-XXXXXX)"

  cd "$test_dir" && PATH="${fake_bin_dir}:$PATH" NOTIF_LOG="$notif_log" \
    "$FACTORY_BIN" watch 1 > "$outfile" 2>&1 &
  local pid=$!
  sleep 2
  printf '%s' "$new_status" > "${test_dir}/.factory/runs/${run_id}/status"
  sleep 3
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  rm -f "$outfile"
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_complete_notification_includes_run_id_and_status() {
  TEST_DIR="$(mktemp -d -t factory-test-notif-XXXXXX)"
  NOTIF_LOG="${TEST_DIR}/notif.log"
  FAKE_BIN_DIR="$(setup_fake_osascript "$TEST_DIR")"

  mkdir -p "${TEST_DIR}/.factory/runs/run-abc123/reviews"
  printf 'executing' > "${TEST_DIR}/.factory/runs/run-abc123/status"
  printf '# Brief\n\nDeploy feature X' > "${TEST_DIR}/.factory/runs/run-abc123/brief.md"
  printf 'local' > "${TEST_DIR}/.factory/runs/run-abc123/runtime"
  printf '1 0 10 executing\n2 0 12 complete\n' > "${TEST_DIR}/.factory/runs/run-abc123/sessions.log"
  printf -- '---\nVerdict: pass\n---\n' > "${TEST_DIR}/.factory/runs/run-abc123/reviews/review-tests.md"

  watch_with_status_change "$TEST_DIR" "$FAKE_BIN_DIR" "run-abc123" "complete" "$NOTIF_LOG"

  RESULT=0
  assert_notif_contains "$NOTIF_LOG" "run-abc123" || RESULT=1
  assert_notif_contains "$NOTIF_LOG" "complete" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_complete_notification_includes_brief_summary() {
  TEST_DIR="$(mktemp -d -t factory-test-notif-XXXXXX)"
  NOTIF_LOG="${TEST_DIR}/notif.log"
  FAKE_BIN_DIR="$(setup_fake_osascript "$TEST_DIR")"

  mkdir -p "${TEST_DIR}/.factory/runs/run-brief/reviews"
  printf 'executing' > "${TEST_DIR}/.factory/runs/run-brief/status"
  printf '# Brief\n\nAdd retry logic to webhook sender' > "${TEST_DIR}/.factory/runs/run-brief/brief.md"
  printf 'local' > "${TEST_DIR}/.factory/runs/run-brief/runtime"
  printf -- '---\nVerdict: pass\n---\n' > "${TEST_DIR}/.factory/runs/run-brief/reviews/review-tests.md"

  watch_with_status_change "$TEST_DIR" "$FAKE_BIN_DIR" "run-brief" "complete" "$NOTIF_LOG"

  RESULT=0
  assert_notif_contains "$NOTIF_LOG" "Add retry logic to webhook sender" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_complete_notification_includes_review_verdict() {
  TEST_DIR="$(mktemp -d -t factory-test-notif-XXXXXX)"
  NOTIF_LOG="${TEST_DIR}/notif.log"
  FAKE_BIN_DIR="$(setup_fake_osascript "$TEST_DIR")"

  mkdir -p "${TEST_DIR}/.factory/runs/run-rev/reviews"
  printf 'executing' > "${TEST_DIR}/.factory/runs/run-rev/status"
  printf '# Brief\n\nFix auth bug' > "${TEST_DIR}/.factory/runs/run-rev/brief.md"
  printf 'local' > "${TEST_DIR}/.factory/runs/run-rev/runtime"
  printf '1 0 10 executing\n2 0 12 complete\n' > "${TEST_DIR}/.factory/runs/run-rev/sessions.log"
  printf -- '---\nVerdict: pass\n---\n' > "${TEST_DIR}/.factory/runs/run-rev/reviews/review-tests.md"
  printf -- '---\nVerdict: pass\n---\n' > "${TEST_DIR}/.factory/runs/run-rev/reviews/review-arch.md"

  watch_with_status_change "$TEST_DIR" "$FAKE_BIN_DIR" "run-rev" "complete" "$NOTIF_LOG"

  RESULT=0
  assert_notif_contains "$NOTIF_LOG" "reviews passed" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_failed_notification_includes_brief() {
  TEST_DIR="$(mktemp -d -t factory-test-notif-XXXXXX)"
  NOTIF_LOG="${TEST_DIR}/notif.log"
  FAKE_BIN_DIR="$(setup_fake_osascript "$TEST_DIR")"

  mkdir -p "${TEST_DIR}/.factory/runs/run-broken"
  printf 'executing' > "${TEST_DIR}/.factory/runs/run-broken/status"
  printf '# Brief\n\nRefactor database layer' > "${TEST_DIR}/.factory/runs/run-broken/brief.md"
  printf 'local' > "${TEST_DIR}/.factory/runs/run-broken/runtime"

  watch_with_status_change "$TEST_DIR" "$FAKE_BIN_DIR" "run-broken" "failed" "$NOTIF_LOG"

  RESULT=0
  assert_notif_contains "$NOTIF_LOG" "run-broken" || RESULT=1
  assert_notif_contains "$NOTIF_LOG" "failed" || RESULT=1
  assert_notif_contains "$NOTIF_LOG" "Refactor database layer" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_complete_notification_includes_session_count() {
  TEST_DIR="$(mktemp -d -t factory-test-notif-XXXXXX)"
  NOTIF_LOG="${TEST_DIR}/notif.log"
  FAKE_BIN_DIR="$(setup_fake_osascript "$TEST_DIR")"

  mkdir -p "${TEST_DIR}/.factory/runs/run-sess/reviews"
  mkdir -p "${TEST_DIR}/.factory/runs/run-sess/sessions/session-1"
  mkdir -p "${TEST_DIR}/.factory/runs/run-sess/sessions/session-2"
  mkdir -p "${TEST_DIR}/.factory/runs/run-sess/sessions/session-3"
  printf 'executing' > "${TEST_DIR}/.factory/runs/run-sess/status"
  printf '# Brief\n\nBuild new widget' > "${TEST_DIR}/.factory/runs/run-sess/brief.md"
  printf 'local' > "${TEST_DIR}/.factory/runs/run-sess/runtime"
  printf -- '---\nVerdict: pass\n---\n' > "${TEST_DIR}/.factory/runs/run-sess/reviews/review-tests.md"

  watch_with_status_change "$TEST_DIR" "$FAKE_BIN_DIR" "run-sess" "complete" "$NOTIF_LOG"

  RESULT=0
  assert_notif_contains "$NOTIF_LOG" "3 sessions" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_needs_user_notification_includes_handoff_content() {
  TEST_DIR="$(mktemp -d -t factory-test-notif-XXXXXX)"
  NOTIF_LOG="${TEST_DIR}/notif.log"
  FAKE_BIN_DIR="$(setup_fake_osascript "$TEST_DIR")"

  mkdir -p "${TEST_DIR}/.factory/runs/run-handoff"
  printf 'executing' > "${TEST_DIR}/.factory/runs/run-handoff/status"
  printf '# Brief\n\nFix login page CSS' > "${TEST_DIR}/.factory/runs/run-handoff/brief.md"
  printf 'local' > "${TEST_DIR}/.factory/runs/run-handoff/runtime"
  cat > "${TEST_DIR}/.factory/runs/run-handoff/handoff.md" << 'HANDOFF'
## Run run-handoff
Brief: Fix login page CSS
Status: needs-user

### Completed
- Investigated CSS issues

### Open questions
- Should the button be blue or green?

### Next steps
- Apply chosen color
HANDOFF

  watch_with_status_change "$TEST_DIR" "$FAKE_BIN_DIR" "run-handoff" "needs-user" "$NOTIF_LOG"

  RESULT=0
  assert_notif_contains "$NOTIF_LOG" "Should the button be blue or green?" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-notification-content\n\n'

run_test "complete notification includes run ID and status" test_complete_notification_includes_run_id_and_status
run_test "complete notification includes brief summary" test_complete_notification_includes_brief_summary
run_test "complete notification includes review verdict" test_complete_notification_includes_review_verdict
run_test "failed notification includes brief summary" test_failed_notification_includes_brief
run_test "complete notification includes session count" test_complete_notification_includes_session_count
run_test "needs-user notification includes handoff content" test_needs_user_notification_includes_handoff_content

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
