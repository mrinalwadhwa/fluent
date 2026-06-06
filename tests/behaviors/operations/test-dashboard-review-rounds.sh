#!/usr/bin/env bash
# test-dashboard-review-rounds - Verify dashboard review-round rendering.
#
# Tests:
#   - Reviewing status shows active reviewer work before transcripts exist
#   - Archived review artifacts do not drive current reviewer verdict tabs
#   - Archived reviewer transcripts do not create current reviewer tabs
#   - Stale reviewer tabs disappear after top-level transcripts are archived
#
# Usage:
#   tests/behaviors/operations/test-dashboard-review-rounds.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

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

capture_dashboard() {
  PROJECT_PATH="$1"
  RUN_ID="$2"
  OUTPUT_FILE="$(mktemp -t factory-dashboard-output-XXXXXX)"

  (
    sleep 1
    printf 'q'
  ) | FACTORY_DASH_BIN="$FACTORY_BIN" \
      FACTORY_DASH_PROJECT="$PROJECT_PATH" \
      FACTORY_DASH_RUN="$RUN_ID" \
      TERM=xterm \
      script -q "$OUTPUT_FILE" sh -c 'stty rows 30 cols 120; "$FACTORY_DASH_BIN" dashboard --run-id "$FACTORY_DASH_RUN" "$FACTORY_DASH_PROJECT"' >/dev/null 2>&1 || true

  cat "$OUTPUT_FILE"
  rm -f "$OUTPUT_FILE"
}

clean_dashboard_output() {
  perl -pe 's/\e\[[0-9;?]*[ -\/]*[@-~]//g; s/\r/\n/g'
}

write_review_artifacts() {
  REVIEWS_DIR="$1"
  REVIEWER="$2"
  VERDICT="$3"
  mkdir -p "$REVIEWS_DIR"
  printf '{}\n' > "${REVIEWS_DIR}/transcript-${REVIEWER}.jsonl"
  printf 'Verdict: %s\n' "$VERDICT" > "${REVIEWS_DIR}/review-${REVIEWER}.md"
}

test_reviewing_status_shows_active_work_before_transcripts() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-rounds-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/reviewing-run/reviews"
  printf 'reviewing' > "${TEST_DIR}/.factory/runs/reviewing-run/status"
  printf 'Reviewing brief' > "${TEST_DIR}/.factory/runs/reviewing-run/brief.md"

  OUTPUT="$(capture_dashboard "$TEST_DIR" reviewing-run)"
  CLEAN_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output)"

  RESULT=0
  if echo "$CLEAN_OUTPUT" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked for reviewing status\n'
    RESULT=1
  fi
  if ! echo "$CLEAN_OUTPUT" | grep -q "Reviewing"; then
    printf '    FAIL: expected header to show reviewing as active work\n'
    RESULT=1
  fi
  if ! echo "$CLEAN_OUTPUT" | grep -Eq '⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇'; then
    printf '    FAIL: expected reviewing header to show spinner frames\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_archived_reviews_do_not_drive_current_verdict() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-rounds-XXXXXX)"
  RUN_DIR="${TEST_DIR}/.factory/runs/round-run"
  mkdir -p "${RUN_DIR}/reviews/round-1"
  printf 'reviewing' > "${RUN_DIR}/status"
  printf 'Round brief' > "${RUN_DIR}/brief.md"
  write_review_artifacts "${RUN_DIR}/reviews/round-1" behaviors fail
  write_review_artifacts "${RUN_DIR}/reviews" behaviors pass

  OUTPUT="$(capture_dashboard "$TEST_DIR" round-run)"
  CLEAN_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output)"

  RESULT=0
  if ! echo "$CLEAN_OUTPUT" | grep -q "✓ behaviors"; then
    printf '    FAIL: expected current top-level pass verdict tab\n'
    RESULT=1
  fi
  if echo "$CLEAN_OUTPUT" | grep -q "✗ behaviors"; then
    printf '    FAIL: archived fail verdict appeared as current\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_archived_transcripts_do_not_create_current_tabs() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-rounds-XXXXXX)"
  RUN_DIR="${TEST_DIR}/.factory/runs/archived-run"
  mkdir -p "${RUN_DIR}/reviews/round-1"
  printf 'reviewing' > "${RUN_DIR}/status"
  printf 'Archived brief' > "${RUN_DIR}/brief.md"
  write_review_artifacts "${RUN_DIR}/reviews/round-1" behaviors fail

  OUTPUT="$(capture_dashboard "$TEST_DIR" archived-run)"
  CLEAN_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output)"

  RESULT=0
  if echo "$CLEAN_OUTPUT" | grep -q "behaviors"; then
    printf '    FAIL: archived reviewer appeared as a current tab\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_stale_reviewer_tabs_disappear_when_top_level_transcripts_are_archived() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-rounds-XXXXXX)"
  RUN_DIR="${TEST_DIR}/.factory/runs/stale-run"
  mkdir -p "${RUN_DIR}/reviews"
  printf 'reviewing' > "${RUN_DIR}/status"
  printf 'Stale tab brief' > "${RUN_DIR}/brief.md"
  write_review_artifacts "${RUN_DIR}/reviews" behaviors fail

  RESULT=0
  if ! python3 - "$FACTORY_BIN" "$TEST_DIR" "$RUN_DIR" <<'PY'
import os
import pty
import re
import select
import shutil
import subprocess
import sys
import fcntl
import struct
import termios
import time

factory_bin, test_dir, run_dir = sys.argv[1:]
rows, cols = 30, 120
grid = [[" " for _ in range(cols)] for _ in range(rows)]
row = 0
col = 0


def clear_screen():
    for y in range(rows):
        for x in range(cols):
            grid[y][x] = " "


def clear_line():
    for x in range(col, cols):
        grid[row][x] = " "


def put_char(ch):
    global row, col
    if col >= cols:
        col = 0
        row = min(row + 1, rows - 1)
    grid[row][col] = ch
    col += 1


def apply_csi(params, final):
    global row, col
    params = params.lstrip("?")
    values = [int(p) if p else 0 for p in params.split(";") if p or params == ""]
    first = values[0] if values else 0
    if final in ("H", "f"):
        row = max(0, min((values[0] if len(values) > 0 and values[0] else 1) - 1, rows - 1))
        col = max(0, min((values[1] if len(values) > 1 and values[1] else 1) - 1, cols - 1))
    elif final == "J" and first in (0, 2, 3):
        clear_screen()
        row = 0
        col = 0
    elif final == "K":
        clear_line()
    elif final == "A":
        row = max(0, row - max(first, 1))
    elif final == "B":
        row = min(rows - 1, row + max(first, 1))
    elif final == "C":
        col = min(cols - 1, col + max(first, 1))
    elif final == "D":
        col = max(0, col - max(first, 1))


ansi_re = re.compile(r"\x1b\[([0-9;?]*)([@-~])")


def feed(data):
    global row, col
    text = data.decode("utf-8", "ignore")
    i = 0
    while i < len(text):
        if text[i] == "\x1b":
            match = ansi_re.match(text, i)
            if match:
                apply_csi(match.group(1), match.group(2))
                i = match.end()
            else:
                i += 1
            continue
        ch = text[i]
        if ch == "\r":
            col = 0
        elif ch == "\n":
            row = min(row + 1, rows - 1)
        elif ch == "\b":
            col = max(0, col - 1)
        elif ord(ch) >= 32:
            put_char(ch)
        i += 1


def screen_text():
    return "\n".join("".join(line).rstrip() for line in grid)


def wait_for(predicate, description):
    deadline = time.time() + 8
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.05)
        if ready:
            try:
                feed(os.read(master, 65536))
            except OSError:
                break
        text = screen_text()
        if predicate(text):
            return True
    print(f"    FAIL: timed out waiting for {description}", file=sys.stderr)
    print(screen_text(), file=sys.stderr)
    return False


master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
env = os.environ.copy()
env["TERM"] = "xterm"
proc = subprocess.Popen(
    [factory_bin, "dashboard", "--run-id", "stale-run", test_dir],
    stdin=slave,
    stdout=slave,
    stderr=slave,
    env=env,
    close_fds=True,
)
os.close(slave)

try:
    if not wait_for(lambda text: "✗ behaviors" in text, "failed reviewer tab"):
        sys.exit(1)

    archive_dir = os.path.join(run_dir, "reviews", "round-1")
    os.makedirs(archive_dir, exist_ok=True)
    shutil.move(
        os.path.join(run_dir, "reviews", "review-behaviors.md"),
        os.path.join(archive_dir, "review-behaviors.md"),
    )
    shutil.move(
        os.path.join(run_dir, "reviews", "transcript-behaviors.jsonl"),
        os.path.join(archive_dir, "transcript-behaviors.jsonl"),
    )

    if not wait_for(
        lambda text: "Reviewing" in text and "behaviors" not in text,
        "archived reviewer tab removal",
    ):
        sys.exit(1)
finally:
    try:
        os.write(master, b"q")
    except OSError:
        pass
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    os.close(master)
PY
  then
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

printf 'test-dashboard-review-rounds\n\n'

run_test "reviewing status shows active work before transcripts" test_reviewing_status_shows_active_work_before_transcripts
run_test "archived reviews do not drive current verdict" test_archived_reviews_do_not_drive_current_verdict
run_test "archived transcripts do not create current tabs" test_archived_transcripts_do_not_create_current_tabs
run_test "stale reviewer tabs disappear when top-level transcripts are archived" test_stale_reviewer_tabs_disappear_when_top_level_transcripts_are_archived

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
