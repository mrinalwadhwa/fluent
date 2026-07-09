#!/usr/bin/env bash
# test-fargate-entrypoint-codex — Verify Codex-specific Fargate entrypoint
# auth validation, billing guardrails, and coder dispatch.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/../../.." && pwd)"
ENTRYPOINT="${PROJECT_DIR}/infrastructure/run/entrypoint.sh"
PASS=0
FAIL=0
ERRORS=""

run_test() {
  local name="$1"
  printf '  %s ... ' "$name"
  if ( eval "$2" ) 2>&1; then
    printf 'PASS\n'
    PASS=$((PASS + 1))
  else
    printf '\n'
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  - ${name}"
  fi
}

setup_entrypoint_test() {
  TEST_DIR="$(mktemp -d -t fluent-codex-entrypoint-XXXXXX)"

  MOCK_BIN="${TEST_DIR}/bin"
  WORKTREES="${TEST_DIR}/worktrees"
  mkdir -p "$MOCK_BIN" "$WORKTREES"

  cat > "$MOCK_BIN/fluent" <<'FLUENT'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'fluent-bin=%s\n' "$0"
  printf '%s\n' "$@"
} > "$MOCK_FLUENT_ARGS"
printf 'OPENAI_API_KEY=%s\n' "${OPENAI_API_KEY:-UNSET}" > "$MOCK_FLUENT_ENV"
FLUENT
  chmod +x "$MOCK_BIN/fluent"

  cat > "$MOCK_BIN/codex" <<'CODEX'
#!/usr/bin/env bash
echo "codex mock"
CODEX
  chmod +x "$MOCK_BIN/codex"

  cat > "$MOCK_BIN/aws" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "s3" ] && [ "${2:-}" = "cp" ]; then
  shift 2
  while [ $# -gt 0 ]; do
    case "$1" in
      --region) shift 2 ;;
      --no-progress) shift ;;
      *) break ;;
    esac
  done
  src="${1:-}"
  dst="${2:-}"
  if [[ "$src" == s3://* ]] && [[ "$dst" != s3://* ]]; then
    cp "$MOCK_WORKSPACE_IN" "$dst"
  elif [[ "$dst" == s3://* ]] && [[ "$src" != s3://* ]]; then
    cp "$src" "$MOCK_WORKSPACE_OUT"
  fi
  exit 0
fi
exit 1
SH
  chmod +x "$MOCK_BIN/aws"

  local workspace="${TEST_DIR}/workspace-src/testproject"
  mkdir -p "$workspace"
  printf 'test\n' > "$workspace/README.md"
  MOCK_WORKSPACE_IN="${TEST_DIR}/workspace-in.tar"
  MOCK_WORKSPACE_OUT="${TEST_DIR}/workspace-out.tar"
  tar cf "$MOCK_WORKSPACE_IN" -C "${TEST_DIR}/workspace-src" testproject
  MOCK_FLUENT_ARGS="${TEST_DIR}/fluent-args"
  MOCK_FLUENT_ENV="${TEST_DIR}/fluent-env"
}

cleanup_entrypoint_test() {
  rm -rf "$TEST_DIR"
}

# Common env for full-flow tests (codex or claude).
run_entrypoint_full() {
  local coder_env=("$@")
  HOME="${TEST_DIR}/fakehome" \
  PATH="${MOCK_BIN}:${PATH}" \
  FLUENT_WORKTREES_ROOT="$WORKTREES" \
  FLUENT_WORK_ITEM_ID="w1" \
  FLUENT_WORK_ATTEMPT_ID="a1" \
  FLUENT_PROJECT_NAME="testproject" \
  FLUENT_S3_BUCKET="bucket" \
  FLUENT_REGION="us-west-1" \
  FLUENT_BIN="$MOCK_BIN/fluent" \
  MOCK_WORKSPACE_IN="$MOCK_WORKSPACE_IN" \
  MOCK_WORKSPACE_OUT="$MOCK_WORKSPACE_OUT" \
  MOCK_FLUENT_ARGS="$MOCK_FLUENT_ARGS" \
  MOCK_FLUENT_ENV="$MOCK_FLUENT_ENV" \
  "${coder_env[@]}" \
    bash "$ENTRYPOINT"
}

test_codex_writes_auth_json_and_unsets_openai_key() {
  setup_entrypoint_test
  local FAKE_HOME="${TEST_DIR}/fakehome"
  mkdir -p "$FAKE_HOME"

  local AUTH_JSON='{"auth_mode":"chatgpt","refresh_token":"tok123"}'

  run_entrypoint_full \
    env FLUENT_CODER="codex" \
    CODEX_AUTH_JSON="$AUTH_JSON" \
    OPENAI_API_KEY="should-be-unset"

  RESULT=0

  if [ ! -f "${FAKE_HOME}/.codex/auth.json" ]; then
    printf '    FAIL: auth.json not created\n'
    RESULT=1
  elif [ "$(cat "${FAKE_HOME}/.codex/auth.json")" != "$AUTH_JSON" ]; then
    printf '    FAIL: auth.json content mismatch\n'
    RESULT=1
  fi

  local perms
  perms="$(stat -f '%Lp' "${FAKE_HOME}/.codex/auth.json" 2>/dev/null || stat -c '%a' "${FAKE_HOME}/.codex/auth.json" 2>/dev/null)"
  if [ "$perms" != "600" ]; then
    printf '    FAIL: auth.json permissions are %s, expected 600\n' "$perms"
    RESULT=1
  fi

  if ! grep -q 'OPENAI_API_KEY=UNSET' "$MOCK_FLUENT_ENV"; then
    printf '    FAIL: OPENAI_API_KEY was not unset before fluent binary\n'
    RESULT=1
  fi

  if ! grep -q -- '--coder' "$MOCK_FLUENT_ARGS" || ! grep -q 'codex' "$MOCK_FLUENT_ARGS"; then
    printf '    FAIL: fluent was not called with --coder codex\n'
    RESULT=1
  fi

  cleanup_entrypoint_test
  return $RESULT
}

test_codex_missing_env_var_fails() {
  setup_entrypoint_test
  mkdir -p "${TEST_DIR}/fakehome"

  set +e
  OUTPUT="$(HOME="${TEST_DIR}/fakehome" \
    PATH="${MOCK_BIN}:${PATH}" \
    FLUENT_WORKTREES_ROOT="$WORKTREES" \
    FLUENT_CODER="codex" \
    FLUENT_WORK_ITEM_ID="w1" \
    FLUENT_WORK_ATTEMPT_ID="a1" \
    FLUENT_PROJECT_NAME="testproject" \
    FLUENT_S3_BUCKET="bucket" \
    FLUENT_REGION="us-west-1" \
    bash "$ENTRYPOINT" 2>&1)"
  STATUS=$?
  set -e

  RESULT=0
  if [ "$STATUS" -eq 0 ]; then
    printf '    FAIL: entrypoint succeeded without CODEX_AUTH_JSON\n'
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -q "CODEX_AUTH_JSON is not set"; then
    printf '    FAIL: expected error about CODEX_AUTH_JSON, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_entrypoint_test
  return $RESULT
}

test_codex_apikey_auth_mode_rejected() {
  setup_entrypoint_test
  mkdir -p "${TEST_DIR}/fakehome"

  set +e
  OUTPUT="$(HOME="${TEST_DIR}/fakehome" \
    PATH="${MOCK_BIN}:${PATH}" \
    FLUENT_WORKTREES_ROOT="$WORKTREES" \
    FLUENT_CODER="codex" \
    CODEX_AUTH_JSON='{"auth_mode":"apikey","api_key":"sk-test"}' \
    FLUENT_WORK_ITEM_ID="w1" \
    FLUENT_WORK_ATTEMPT_ID="a1" \
    FLUENT_PROJECT_NAME="testproject" \
    FLUENT_S3_BUCKET="bucket" \
    FLUENT_REGION="us-west-1" \
    bash "$ENTRYPOINT" 2>&1)"
  STATUS=$?
  set -e

  RESULT=0
  if [ "$STATUS" -eq 0 ]; then
    printf '    FAIL: entrypoint succeeded with apikey auth_mode\n'
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -q "auth_mode=chatgpt"; then
    printf '    FAIL: expected error about auth_mode=chatgpt, got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if [ -f "$MOCK_FLUENT_ARGS" ]; then
    printf '    FAIL: fluent binary was invoked despite auth_mode rejection\n'
    RESULT=1
  fi

  cleanup_entrypoint_test
  return $RESULT
}

test_codex_config_toml_apikey_rejected() {
  setup_entrypoint_test
  mkdir -p "${TEST_DIR}/fakehome/.codex"
  printf 'preferred_auth_method = "apikey"\n' > "${TEST_DIR}/fakehome/.codex/config.toml"

  set +e
  OUTPUT="$(HOME="${TEST_DIR}/fakehome" \
    PATH="${MOCK_BIN}:${PATH}" \
    FLUENT_WORKTREES_ROOT="$WORKTREES" \
    FLUENT_CODER="codex" \
    CODEX_AUTH_JSON='{"auth_mode":"chatgpt","refresh_token":"tok"}' \
    FLUENT_WORK_ITEM_ID="w1" \
    FLUENT_WORK_ATTEMPT_ID="a1" \
    FLUENT_PROJECT_NAME="testproject" \
    FLUENT_S3_BUCKET="bucket" \
    FLUENT_REGION="us-west-1" \
    bash "$ENTRYPOINT" 2>&1)"
  STATUS=$?
  set -e

  RESULT=0
  if [ "$STATUS" -eq 0 ]; then
    printf '    FAIL: entrypoint succeeded with apikey config.toml\n'
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -q "preferred_auth_method=apikey"; then
    printf '    FAIL: expected error about preferred_auth_method=apikey, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_entrypoint_test
  return $RESULT
}

test_codex_openai_api_key_unset_in_binary_env() {
  setup_entrypoint_test
  mkdir -p "${TEST_DIR}/fakehome"

  run_entrypoint_full \
    env FLUENT_CODER="codex" \
    CODEX_AUTH_JSON='{"auth_mode":"chatgpt","refresh_token":"tok"}' \
    OPENAI_API_KEY="leaked-key"

  RESULT=0
  if ! grep -q 'OPENAI_API_KEY=UNSET' "$MOCK_FLUENT_ENV"; then
    printf '    FAIL: OPENAI_API_KEY was not unset in fluent binary env\n'
    RESULT=1
  fi

  cleanup_entrypoint_test
  return $RESULT
}

test_claude_path_unchanged() {
  setup_entrypoint_test
  mkdir -p "${TEST_DIR}/fakehome"

  run_entrypoint_full \
    env FLUENT_CODER="claude" \
    CLAUDE_CODE_OAUTH_TOKEN="test-token"

  RESULT=0
  if ! grep -q -- '--coder' "$MOCK_FLUENT_ARGS" || ! grep -q 'claude' "$MOCK_FLUENT_ARGS"; then
    printf '    FAIL: fluent was not called with --coder claude\n'
    RESULT=1
  fi

  cleanup_entrypoint_test
  return $RESULT
}

test_default_coder_is_claude() {
  setup_entrypoint_test
  mkdir -p "${TEST_DIR}/fakehome"

  HOME="${TEST_DIR}/fakehome" \
  PATH="${MOCK_BIN}:${PATH}" \
  FLUENT_WORKTREES_ROOT="$WORKTREES" \
  CLAUDE_CODE_OAUTH_TOKEN="test-token" \
  FLUENT_WORK_ITEM_ID="w1" \
  FLUENT_WORK_ATTEMPT_ID="a1" \
  FLUENT_PROJECT_NAME="testproject" \
  FLUENT_S3_BUCKET="bucket" \
  FLUENT_REGION="us-west-1" \
  FLUENT_BIN="$MOCK_BIN/fluent" \
  MOCK_WORKSPACE_IN="$MOCK_WORKSPACE_IN" \
  MOCK_WORKSPACE_OUT="$MOCK_WORKSPACE_OUT" \
  MOCK_FLUENT_ARGS="$MOCK_FLUENT_ARGS" \
  MOCK_FLUENT_ENV="$MOCK_FLUENT_ENV" \
    bash "$ENTRYPOINT"

  RESULT=0
  if ! grep -q 'claude' "$MOCK_FLUENT_ARGS"; then
    printf '    FAIL: fluent was not called with --coder claude (default)\n'
    RESULT=1
  fi

  cleanup_entrypoint_test
  return $RESULT
}

test_unknown_coder_fails() {
  setup_entrypoint_test
  mkdir -p "${TEST_DIR}/fakehome"

  set +e
  OUTPUT="$(HOME="${TEST_DIR}/fakehome" \
    PATH="${MOCK_BIN}:${PATH}" \
    FLUENT_WORKTREES_ROOT="$WORKTREES" \
    FLUENT_CODER="gpt5" \
    FLUENT_WORK_ITEM_ID="w1" \
    FLUENT_WORK_ATTEMPT_ID="a1" \
    FLUENT_PROJECT_NAME="testproject" \
    FLUENT_S3_BUCKET="bucket" \
    FLUENT_REGION="us-west-1" \
    bash "$ENTRYPOINT" 2>&1)"
  STATUS=$?
  set -e

  RESULT=0
  if [ "$STATUS" -eq 0 ]; then
    printf '    FAIL: entrypoint succeeded with unknown coder\n'
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -q "Unsupported FLUENT_CODER"; then
    printf '    FAIL: expected error about unsupported coder, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_entrypoint_test
  return $RESULT
}

printf 'test-fargate-entrypoint-codex\n\n'

run_test "codex writes auth.json and unsets OPENAI_API_KEY" test_codex_writes_auth_json_and_unsets_openai_key
run_test "codex missing CODEX_AUTH_JSON fails" test_codex_missing_env_var_fails
run_test "codex apikey auth_mode rejected" test_codex_apikey_auth_mode_rejected
run_test "codex config.toml apikey preference rejected" test_codex_config_toml_apikey_rejected
run_test "codex OPENAI_API_KEY unset in binary env" test_codex_openai_api_key_unset_in_binary_env
run_test "claude path unchanged" test_claude_path_unchanged
run_test "default coder is claude" test_default_coder_is_claude
run_test "unknown coder fails" test_unknown_coder_fails

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
