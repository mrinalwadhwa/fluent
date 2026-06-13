#!/usr/bin/env bash
# test-keep-awake — Verify factory keep-awake behavior.
#
# Tests the keep-awake toggle using mocked pgrep, launchctl, and kill
# so no real system changes occur. Each test sets HOME to a temp dir
# and prepends mock-bin scripts to PATH.
#
# Covers:
#   - status reports "off" when no caffeinate process is running
#   - status reports "on (caffeinate PID <pid>)" when a process is found
#   - on first invocation installs LaunchAgent and wrapper script
#   - on when already running prints "already on"
#   - off when not running prints "already off"
#   - off when running updates plist and calls bootout
#   - uninstall removes plist and wrapper script
#
# Usage:
#   tests/behaviors/operations/test-keep-awake.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_test_env() {
  TEST_HOME="$(mktemp -d -t factory-keep-awake-XXXXXX)"
  MOCK_DIR="$(mktemp -d -t factory-mock-bin-XXXXXX)"
  MOCK_STATE="$(mktemp -d -t factory-mock-state-XXXXXX)"

  mkdir -p "$TEST_HOME/Library/LaunchAgents"
  mkdir -p "$TEST_HOME/.config/factory"

  # Mock pgrep
  cat > "$MOCK_DIR/pgrep" << 'MOCK_PGREP'
#!/bin/sh
echo "pgrep $@" >> "$FACTORY_MOCK_STATE/calls.log"
if [ "$1" = "-f" ]; then
  if [ -f "$FACTORY_MOCK_STATE/wrapper_pid" ]; then
    cat "$FACTORY_MOCK_STATE/wrapper_pid"
    exit 0
  fi
  exit 1
elif [ "$1" = "-P" ]; then
  if [ -f "$FACTORY_MOCK_STATE/caffeinate_pid" ]; then
    cat "$FACTORY_MOCK_STATE/caffeinate_pid"
    exit 0
  fi
  exit 1
fi
exit 1
MOCK_PGREP
  chmod +x "$MOCK_DIR/pgrep"

  # Mock launchctl
  cat > "$MOCK_DIR/launchctl" << 'MOCK_LAUNCHCTL'
#!/bin/sh
echo "launchctl $@" >> "$FACTORY_MOCK_STATE/calls.log"
exit 0
MOCK_LAUNCHCTL
  chmod +x "$MOCK_DIR/launchctl"

  # Mock kill
  cat > "$MOCK_DIR/kill" << 'MOCK_KILL'
#!/bin/sh
echo "kill $@" >> "$FACTORY_MOCK_STATE/calls.log"
# Simulate process exiting by removing the PID files
if [ "$1" = "-0" ]; then
  if [ -f "$FACTORY_MOCK_STATE/wrapper_pid" ]; then
    exit 0
  fi
  exit 1
fi
if [ "$1" = "-TERM" ]; then
  rm -f "$FACTORY_MOCK_STATE/wrapper_pid" 2>/dev/null
  rm -f "$FACTORY_MOCK_STATE/caffeinate_pid" 2>/dev/null
fi
exit 0
MOCK_KILL
  chmod +x "$MOCK_DIR/kill"

  # Mock id (for UID)
  cat > "$MOCK_DIR/id" << 'MOCK_ID'
#!/bin/sh
echo "501"
MOCK_ID
  chmod +x "$MOCK_DIR/id"

  export HOME="$TEST_HOME"
  export PATH="$MOCK_DIR:$PATH"
  export FACTORY_MOCK_STATE="$MOCK_STATE"
}

teardown_test_env() {
  rm -rf "$TEST_HOME" "$MOCK_DIR" "$MOCK_STATE"
}

test_status_off_when_no_process() {
  setup_test_env

  OUTPUT="$("$FACTORY_BIN" keep-awake status 2>&1)"
  STATUS=$?

  RESULT=0
  if [ "$STATUS" -ne 0 ]; then
    printf '    FAIL: expected exit 0, got %s\n' "$STATUS"
    RESULT=1
  fi
  if [ "$OUTPUT" != "off" ]; then
    printf '    FAIL: expected "off", got "%s"\n' "$OUTPUT"
    RESULT=1
  fi

  teardown_test_env
  return $RESULT
}

test_status_on_when_process_running() {
  setup_test_env
  echo "12345" > "$MOCK_STATE/wrapper_pid"
  echo "12346" > "$MOCK_STATE/caffeinate_pid"

  OUTPUT="$("$FACTORY_BIN" keep-awake status 2>&1)"
  STATUS=$?

  RESULT=0
  if [ "$STATUS" -ne 0 ]; then
    printf '    FAIL: expected exit 0, got %s\n' "$STATUS"
    RESULT=1
  fi
  if [ "$OUTPUT" != "on (caffeinate PID 12346)" ]; then
    printf '    FAIL: expected "on (caffeinate PID 12346)", got "%s"\n' "$OUTPUT"
    RESULT=1
  fi

  teardown_test_env
  return $RESULT
}

test_on_first_time_installs_launch_agent() {
  setup_test_env
  # After bootstrap, simulate the process starting
  cat > "$MOCK_DIR/launchctl" << 'MOCK'
#!/bin/sh
echo "launchctl $@" >> "$FACTORY_MOCK_STATE/calls.log"
if [ "$1" = "bootstrap" ]; then
  echo "99901" > "$FACTORY_MOCK_STATE/wrapper_pid"
  echo "99902" > "$FACTORY_MOCK_STATE/caffeinate_pid"
fi
exit 0
MOCK
  chmod +x "$MOCK_DIR/launchctl"

  OUTPUT="$("$FACTORY_BIN" keep-awake on 2>&1)"
  STATUS=$?

  RESULT=0
  if [ "$STATUS" -ne 0 ]; then
    printf '    FAIL: expected exit 0, got %s\n' "$STATUS"
    printf '    OUTPUT: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -q "LaunchAgent installed"; then
    printf '    FAIL: expected LaunchAgent installation message\n'
    printf '    OUTPUT: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -q "keep-awake on (caffeinate PID"; then
    printf '    FAIL: expected "keep-awake on" confirmation\n'
    printf '    OUTPUT: %s\n' "$OUTPUT"
    RESULT=1
  fi
  PLIST="$TEST_HOME/Library/LaunchAgents/com.factory.keep-awake.plist"
  if [ ! -f "$PLIST" ]; then
    printf '    FAIL: plist not written at %s\n' "$PLIST"
    RESULT=1
  fi
  WRAPPER="$TEST_HOME/.config/factory/keep-awake-caffeinate"
  if [ ! -f "$WRAPPER" ]; then
    printf '    FAIL: wrapper script not written at %s\n' "$WRAPPER"
    RESULT=1
  fi
  if ! grep -q "launchctl bootstrap" "$MOCK_STATE/calls.log" 2>/dev/null; then
    printf '    FAIL: launchctl bootstrap was not called\n'
    RESULT=1
  fi

  teardown_test_env
  return $RESULT
}

test_on_already_running_prints_already_on() {
  setup_test_env
  echo "55555" > "$MOCK_STATE/wrapper_pid"
  echo "55556" > "$MOCK_STATE/caffeinate_pid"

  OUTPUT="$("$FACTORY_BIN" keep-awake on 2>&1)"
  STATUS=$?

  RESULT=0
  if [ "$STATUS" -ne 0 ]; then
    printf '    FAIL: expected exit 0, got %s\n' "$STATUS"
    RESULT=1
  fi
  if [ "$OUTPUT" != "keep-awake already on (caffeinate PID 55556)" ]; then
    printf '    FAIL: expected already-on message, got "%s"\n' "$OUTPUT"
    RESULT=1
  fi

  teardown_test_env
  return $RESULT
}

test_off_when_already_off() {
  setup_test_env

  OUTPUT="$("$FACTORY_BIN" keep-awake off 2>&1)"
  STATUS=$?

  RESULT=0
  if [ "$STATUS" -ne 0 ]; then
    printf '    FAIL: expected exit 0, got %s\n' "$STATUS"
    RESULT=1
  fi
  if [ "$OUTPUT" != "keep-awake already off" ]; then
    printf '    FAIL: expected "keep-awake already off", got "%s"\n' "$OUTPUT"
    RESULT=1
  fi

  teardown_test_env
  return $RESULT
}

test_off_when_running() {
  setup_test_env
  echo "77777" > "$MOCK_STATE/wrapper_pid"
  echo "77778" > "$MOCK_STATE/caffeinate_pid"
  # Create existing plist with KeepAlive=true
  PLIST="$TEST_HOME/Library/LaunchAgents/com.factory.keep-awake.plist"
  WRAPPER="$TEST_HOME/.config/factory/keep-awake-caffeinate"
  cat > "$PLIST" << 'XML'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.factory.keep-awake</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string>
        <string>/tmp/wrapper</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
XML

  OUTPUT="$("$FACTORY_BIN" keep-awake off 2>&1)"
  STATUS=$?

  RESULT=0
  if [ "$STATUS" -ne 0 ]; then
    printf '    FAIL: expected exit 0, got %s\n' "$STATUS"
    printf '    OUTPUT: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if [ "$OUTPUT" != "keep-awake off" ]; then
    printf '    FAIL: expected "keep-awake off", got "%s"\n' "$OUTPUT"
    RESULT=1
  fi
  if ! grep -q "launchctl bootout" "$MOCK_STATE/calls.log" 2>/dev/null; then
    printf '    FAIL: launchctl bootout was not called\n'
    RESULT=1
  fi
  # Verify plist was rewritten with KeepAlive=false
  if grep -q "<key>KeepAlive</key>" "$PLIST" && grep -A1 "<key>KeepAlive</key>" "$PLIST" | grep -q "<true/>"; then
    printf '    FAIL: plist still has KeepAlive=true after off\n'
    RESULT=1
  fi

  teardown_test_env
  return $RESULT
}

test_uninstall_removes_plist_and_wrapper() {
  setup_test_env
  PLIST="$TEST_HOME/Library/LaunchAgents/com.factory.keep-awake.plist"
  WRAPPER="$TEST_HOME/.config/factory/keep-awake-caffeinate"
  echo "test plist" > "$PLIST"
  echo "test wrapper" > "$WRAPPER"

  OUTPUT="$("$FACTORY_BIN" keep-awake uninstall 2>&1)"
  STATUS=$?

  RESULT=0
  if [ "$STATUS" -ne 0 ]; then
    printf '    FAIL: expected exit 0, got %s\n' "$STATUS"
    printf '    OUTPUT: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if [ -f "$PLIST" ]; then
    printf '    FAIL: plist still exists after uninstall\n'
    RESULT=1
  fi
  if [ -f "$WRAPPER" ]; then
    printf '    FAIL: wrapper still exists after uninstall\n'
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -q "uninstalled"; then
    printf '    FAIL: expected uninstall confirmation, got "%s"\n' "$OUTPUT"
    RESULT=1
  fi

  teardown_test_env
  return $RESULT
}

test_uninstall_already_uninstalled() {
  setup_test_env

  OUTPUT="$("$FACTORY_BIN" keep-awake uninstall 2>&1)"
  STATUS=$?

  RESULT=0
  if [ "$STATUS" -ne 0 ]; then
    printf '    FAIL: expected exit 0, got %s\n' "$STATUS"
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -q "already uninstalled"; then
    printf '    FAIL: expected already-uninstalled message, got "%s"\n' "$OUTPUT"
    RESULT=1
  fi

  teardown_test_env
  return $RESULT
}

printf 'test-keep-awake\n\n'

run_test "status reports off when no caffeinate process is running" test_status_off_when_no_process
run_test "status reports on with caffeinate PID when process is running" test_status_on_when_process_running
run_test "on first invocation installs LaunchAgent and wrapper script" test_on_first_time_installs_launch_agent
run_test "on when already running prints already-on with PID" test_on_already_running_prints_already_on
run_test "off when not running prints already-off" test_off_when_already_off
run_test "off when running calls bootout and updates plist" test_off_when_running
run_test "uninstall removes plist and wrapper script" test_uninstall_removes_plist_and_wrapper
run_test "uninstall when already uninstalled prints already-uninstalled" test_uninstall_already_uninstalled

summarize_and_exit
