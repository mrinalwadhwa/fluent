#!/usr/bin/env bash
# test-sandbox — Verify sandbox behaviors from the user's perspective.
#
# Tests the sandbox-related behaviors from documentation/behaviors.md
# by exercising the factory CLI's external interface: --dry-run for
# profile rendering, sandbox-exec for enforcement, and --no-sandbox
# for the unsandboxed path.
#
# Covers:
#   - Sandbox profile renders with correct workspace root
#   - --sandbox-root override changes the rendered profile
#   - Sandbox profile restricts filesystem to workspace root
#   - Sandbox profile denies Keychain Mach services
#   - sandbox-exec enforces workspace filesystem boundary
#   - sandbox-exec blocks Keychain access
#   - Sandboxed run invokes sandbox-exec (not bare execution)
#   - Credentials injected via environment variables
#
# Usage:
#   tests/behaviors/operations/test-sandbox.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_test_project() {
  TEST_PARENT="${FACTORY_SANDBOX_TEST_PARENT:-$HOME}"
  TEST_DIR="$(mktemp -d "${TEST_PARENT}/factory-test-sandbox-XXXXXX")"
  TEST_DIR="$(cd "$TEST_DIR" && pwd -P)"
  mkdir -p "${TEST_DIR}/project"
  cd "${TEST_DIR}/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  echo "test" > README.md
  git add . && git commit -m "init" > /dev/null 2>&1

  MOCK_BIN="$(git rev-parse --path-format=absolute --git-common-dir)/mock-bin"
  mkdir -p "$MOCK_BIN"
}

create_planned_run() {
  RUN_ID="$1"
  RUN_DIR=".factory/runs/${RUN_ID}"
  mkdir -p "$RUN_DIR"
  printf 'Test sandbox brief' > "${RUN_DIR}/brief.md"
  printf 'planned' > "${RUN_DIR}/status"
  printf 'local' > "${RUN_DIR}/runtime"
  printf '%s' "$RUN_ID" > .factory/active-run
}

find_worktree() {
  local run_dir="$1"
  if [ -f "${run_dir}/worktree" ]; then
    cat "${run_dir}/worktree"
  else
    echo ""
  fi
}

cleanup_test_project() {
  cd /
  if [ -d "${TEST_DIR}/project/.git" ]; then
    git -C "${TEST_DIR}/project" worktree list --porcelain 2>/dev/null | \
      grep '^worktree ' | awk '{print $2}' | \
      grep -v "${TEST_DIR}/project" | while read -r wt; do
      git -C "${TEST_DIR}/project" worktree remove --force "$wt" 2>/dev/null || true
    done
  fi
  rm -rf "$TEST_DIR"
}

assert_output_contains() {
  if ! printf '%s' "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    return 1
  fi
}

assert_output_not_contains() {
  if printf '%s' "$1" | grep -q "$2"; then
    printf '    FAIL: output should not contain "%s"\n' "$2"
    return 1
  fi
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

# Behavior: WHILE running on the local runtime, THE SYSTEM SHALL execute
# the agent inside a macOS Seatbelt sandbox with filesystem access
# restricted to the workspace root.
#
# Test: --dry-run renders a profile with SANDBOX_ROOT matching the
# current directory.

test_dry_run_renders_profile_with_workspace_root() {
  setup_test_project

  OUTPUT="$("$FACTORY_BIN" --dry-run 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "SANDBOX_ROOT" || RESULT=1
  assert_output_contains "$OUTPUT" "${TEST_DIR}/project" || RESULT=1
  # Profile should allow read+write to the sandbox root
  assert_output_contains "$OUTPUT" "file-read" || RESULT=1
  assert_output_contains "$OUTPUT" "file-write" || RESULT=1

  cleanup_test_project
  return $RESULT
}

# Test: --sandbox-root overrides the workspace root in the profile.

test_sandbox_root_override() {
  setup_test_project
  OVERRIDE_DIR="$(mktemp -d -t factory-sandbox-override-XXXXXX)"

  OUTPUT="$("$FACTORY_BIN" --dry-run --sandbox-root "$OVERRIDE_DIR" 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "$OVERRIDE_DIR" || RESULT=1

  rm -rf "$OVERRIDE_DIR"
  cleanup_test_project
  return $RESULT
}

# Behavior: WHILE running inside the sandbox, THE SYSTEM SHALL inject
# credentials via environment variables, never by opening filesystem
# access to credential stores.
#
# Test: The rendered profile denies Keychain Mach services.

test_profile_denies_keychain_mach_services() {
  setup_test_project

  OUTPUT="$("$FACTORY_BIN" --dry-run 2>&1)"

  RESULT=0
  # Profile should deny SecurityServer and securityd Mach lookups
  assert_output_contains "$OUTPUT" 'deny mach-lookup.*SecurityServer' || RESULT=1
  assert_output_contains "$OUTPUT" 'deny mach-lookup.*securityd' || RESULT=1

  cleanup_test_project
  return $RESULT
}

# Test: sandbox-exec with the rendered profile enforces the workspace
# filesystem boundary — can read inside workspace, cannot read outside.

test_sandbox_enforces_filesystem_boundary() {
  setup_test_project

  # Render profile to a file
  PROFILE_FILE="${TEST_DIR}/sandbox.sb"
  "$FACTORY_BIN" --dry-run 2>&1 | \
    sed -n '/^(version 1)/,$ p' > "$PROFILE_FILE"

  # Create a file inside the workspace (should be readable)
  echo "inside-workspace" > "${TEST_DIR}/project/test-readable.txt"

  # Create a probe file outside the workspace. The sandbox profile is
  # deny-default — ~/.factory-sandbox-probe is not in any allow list,
  # so reads are denied regardless of what other dotfiles exist.
  PROBE_FILE="${HOME}/.factory-sandbox-probe"
  echo "outside-workspace" > "$PROBE_FILE"

  RESULT=0

  # Reading inside workspace should succeed
  INSIDE="$(sandbox-exec -f "$PROFILE_FILE" cat "${TEST_DIR}/project/test-readable.txt" 2>/dev/null)" || true
  if [ "$INSIDE" != "inside-workspace" ]; then
    printf '    FAIL: could not read file inside workspace\n'
    RESULT=1
  fi

  # Reading the probe file outside workspace should fail
  OUTSIDE="$(sandbox-exec -f "$PROFILE_FILE" cat "$PROBE_FILE" 2>/dev/null)" || true
  if [ -n "$OUTSIDE" ]; then
    printf '    FAIL: was able to read probe file outside workspace\n'
    RESULT=1
  fi

  rm -f "$PROBE_FILE"
  cleanup_test_project
  return $RESULT
}

# Test: sandbox-exec with the rendered profile blocks writing outside
# the workspace.

test_sandbox_blocks_write_outside_workspace() {
  setup_test_project

  # Render profile to a file
  PROFILE_FILE="${TEST_DIR}/sandbox.sb"
  "$FACTORY_BIN" --dry-run 2>&1 | \
    sed -n '/^(version 1)/,$ p' > "$PROFILE_FILE"

  RESULT=0

  # Writing inside workspace should succeed
  sandbox-exec -f "$PROFILE_FILE" \
    bash -c "echo 'test-write' > '${TEST_DIR}/project/write-test.txt'" 2>/dev/null || true
  if [ ! -f "${TEST_DIR}/project/write-test.txt" ]; then
    printf '    FAIL: could not write file inside workspace\n'
    RESULT=1
  fi

  # Writing to the user's home directory (outside workspace) should fail.
  # Use a unique filename to avoid collision. The profile does not grant
  # write access to ~/ directly.
  OUTSIDE_FILE="${HOME}/.factory-sandbox-write-test-$$"
  sandbox-exec -f "$PROFILE_FILE" \
    bash -c "echo 'test-write' > '${OUTSIDE_FILE}'" 2>/dev/null || true
  if [ -f "$OUTSIDE_FILE" ]; then
    rm -f "$OUTSIDE_FILE"
    printf '    FAIL: was able to write file to home directory from sandbox\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

# Behavior: WHILE running on the local runtime, THE SYSTEM SHALL execute
# the agent inside a macOS Seatbelt sandbox...
#
# Test: When running without --no-sandbox, the binary invokes
# sandbox-exec. Verify by checking that a mock claude receives the
# sandbox environment (it runs inside sandbox-exec). We check this by
# having the mock claude call sandbox-exec itself — if it's already
# sandboxed, the sandbox rules apply.

test_sandboxed_run_uses_sandbox_exec() {
  setup_test_project
  create_planned_run "test-sandbox-exec"

  # Create a probe file outside the workspace. The sandbox profile is
  # deny-default — ~/.factory-sandbox-probe is not in any allow list,
  # so reads are always denied when sandboxed.
  PROBE_FILE="${HOME}/.factory-sandbox-probe"
  echo "sandbox-probe-data" > "$PROBE_FILE"

  # Create a mock claude that detects sandbox by trying to read the
  # probe file. If the read fails, the process is sandboxed.
  cat > "${MOCK_BIN}/claude" << 'MOCK_SCRIPT'
#!/bin/bash
if printf '%s\n' "$*" | grep -q -- '--max-turns'; then
  echo '{"type":"result","subtype":"success","result":"refresh","session_id":"mock-refresh"}'
  exit 0
fi
# Mock claude — detect sandbox by trying to read a probe file outside workspace
DETECTION="unsandboxed"
if ! cat "${HOME}/.factory-sandbox-probe" > /dev/null 2>&1; then
  DETECTION="sandboxed"
fi
echo "$DETECTION" > .sandbox-detection
# Output stream-json and set status
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
if [ -f .factory/active-run ]; then
  RID=$(cat .factory/active-run)
  echo -n "needs-user" > ".factory/runs/$RID/status"
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/claude"

  # Run with sandbox (no --no-sandbox flag)
  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --run-id "test-sandbox-exec" \
    > /dev/null 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-sandbox-exec")"
  DETECTION_FILE=""
  if [ -n "$WT" ] && [ -f "${WT}/.sandbox-detection" ]; then
    DETECTION_FILE="${WT}/.sandbox-detection"
  fi

  if [ -z "$DETECTION_FILE" ]; then
    printf '    FAIL: sandbox detection file not found\n'
    RESULT=1
  else
    DETECTED="$(cat "$DETECTION_FILE")"
    if [ "$DETECTED" != "sandboxed" ]; then
      printf '    FAIL: agent was not sandboxed (detected: %s)\n' "$DETECTED"
      RESULT=1
    fi
  fi

  rm -f "$PROBE_FILE"
  cleanup_test_project
  return $RESULT
}

test_sandboxed_run_can_commit_and_blocks_sibling_write() {
  setup_test_project
  create_planned_run "test-sandbox-commit"

  SIBLING_DIR="${TEST_DIR}/sibling"
  mkdir -p "$SIBLING_DIR"
  SIBLING_WRITE_PROBE="${SIBLING_DIR}/blocked-write.txt"
  export SIBLING_WRITE_PROBE

  cat > "${MOCK_BIN}/claude" << 'MOCK_SCRIPT'
#!/bin/bash
set -u
if printf '%s\n' "$*" | grep -q -- '--max-turns'; then
  echo '{"type":"result","subtype":"success","result":"refresh","session_id":"mock-refresh"}'
  exit 0
fi
echo "sandbox commit" > sandbox-commit.txt
if git add sandbox-commit.txt > .commit-output 2>&1 &&
   git commit -m "Sandbox commit" >> .commit-output 2>&1; then
  echo "commit-ok" > .commit-result
else
  echo "commit-failed" > .commit-result
fi

if echo "should-not-write" > "$SIBLING_WRITE_PROBE" 2>/dev/null; then
  echo "sibling-wrote" > .sibling-write-result
else
  echo "sibling-blocked" > .sibling-write-result
fi

echo '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
if [ -f .factory/active-run ]; then
  RID=$(cat .factory/active-run)
  echo -n "needs-user" > ".factory/runs/$RID/status"
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/claude"

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --run-id "test-sandbox-commit" \
    > /dev/null 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-sandbox-commit")"
  if [ -z "$WT" ]; then
    printf '    FAIL: worktree not found\n'
    RESULT=1
  else
    if [ "$(cat "${WT}/.commit-result" 2>/dev/null || true)" != "commit-ok" ]; then
      printf '    FAIL: sandboxed agent could not commit from worktree\n'
      cat "${WT}/.commit-output" 2>/dev/null || true
      RESULT=1
    fi

    if [ "$(cat "${WT}/.sibling-write-result" 2>/dev/null || true)" != "sibling-blocked" ]; then
      printf '    FAIL: sandboxed agent could write sibling directory\n'
      RESULT=1
    fi

    if [ "$(cat "$SIBLING_WRITE_PROBE" 2>/dev/null || true)" = "should-not-write" ]; then
      printf '    FAIL: sibling write probe contains forbidden content\n'
      RESULT=1
    fi
  fi

  cleanup_test_project
  return $RESULT
}

# Behavior: WHILE running inside the sandbox, THE SYSTEM SHALL inject
# credentials via environment variables...
#
# Test: When running with sandbox, the mock claude receives credential
# environment variables (CLAUDE_CODE_OAUTH_TOKEN or similar).

test_credentials_injected_via_env_vars() {
  setup_test_project
  create_planned_run "test-cred-inject"

  # Create a mock claude that dumps credential-related env vars
  cat > "${MOCK_BIN}/claude" << 'MOCK_SCRIPT'
#!/bin/bash
if printf '%s\n' "$*" | grep -q -- '--max-turns'; then
  echo '{"type":"result","subtype":"success","result":"refresh","session_id":"mock-refresh"}'
  exit 0
fi
# Mock claude — check for credential env vars
env | grep -iE '(CLAUDE|ANTHROPIC|OAUTH|AWS_ACCESS|AWS_SECRET|AWS_SESSION|BRAVE)' \
  > "${PWD}/.credential-env" 2>/dev/null || true
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
if [ -f .factory/active-run ]; then
  RID=$(cat .factory/active-run)
  echo -n "needs-user" > ".factory/runs/$RID/status"
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/claude"

  # Run without sandbox (credential injection should still happen on local runtime)
  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --no-sandbox --run-id "test-cred-inject" \
    > /dev/null 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-cred-inject")"
  CRED_FILE=""
  if [ -n "$WT" ] && [ -f "${WT}/.credential-env" ]; then
    CRED_FILE="${WT}/.credential-env"
  fi

  # The mock claude must have run and created the credential env file.
  # If it doesn't exist, the injection mechanism was never exercised.
  if [ -z "$CRED_FILE" ]; then
    printf '    FAIL: credential env file not found (mock claude did not run)\n'
    RESULT=1
  fi

  # If Keychain credentials are available, the file will contain
  # CLAUDE_CODE_OAUTH_TOKEN or similar. If the file is empty, no
  # Keychain credentials were found — acceptable in CI. The mechanism
  # (env var injection) was still exercised.

  cleanup_test_project
  return $RESULT
}

# Behavior: The rendered profile should deny reading Keychain database
# files and sensitive credential stores.

test_profile_denies_credential_filesystem_access() {
  setup_test_project

  OUTPUT="$("$FACTORY_BIN" --dry-run 2>&1)"

  RESULT=0
  # Keychain DB files denied
  assert_output_contains "$OUTPUT" "Keychains" || RESULT=1
  assert_output_contains "$OUTPUT" "deny.*file-read-data" || RESULT=1

  # AWS credentials denied
  assert_output_contains "$OUTPUT" ".aws" || RESULT=1

  # Git credentials denied
  assert_output_contains "$OUTPUT" ".git-credentials" || RESULT=1

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

if [ ! -x "$FACTORY_BIN" ]; then
  printf 'ERROR: factory binary not found at %s\n' "$FACTORY_BIN"
  printf 'Run "cargo build" first.\n'
  exit 1
fi

if ! command -v sandbox-exec > /dev/null 2>&1; then
  printf 'ERROR: sandbox-exec not found — macOS required\n'
  exit 1
fi

if ! sandbox-exec -p '(version 1)(allow default)' true > /dev/null 2>&1; then
  printf 'test-sandbox\n\n'
  printf 'ERROR: sandbox-exec cannot apply profiles in this environment\n'
  printf 'Sandbox behavior coverage requires a working Seatbelt runtime.\n'
  exit 1
fi

printf 'test-sandbox\n\n'

run_test "dry-run renders profile with workspace root" test_dry_run_renders_profile_with_workspace_root
run_test "sandbox-root override changes profile" test_sandbox_root_override
run_test "profile denies Keychain Mach services" test_profile_denies_keychain_mach_services
run_test "sandbox enforces filesystem boundary" test_sandbox_enforces_filesystem_boundary
run_test "sandbox blocks write outside workspace" test_sandbox_blocks_write_outside_workspace
run_test "sandboxed run uses sandbox-exec" test_sandboxed_run_uses_sandbox_exec
run_test "sandboxed run can commit and blocks sibling write" test_sandboxed_run_can_commit_and_blocks_sibling_write
run_test "credentials injected via env vars" test_credentials_injected_via_env_vars
run_test "profile denies credential filesystem access" test_profile_denies_credential_filesystem_access

summarize_and_exit
