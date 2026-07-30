#!/usr/bin/env bash
# test-codex-executable-boundary - Verify prepared Codex launchers cross Seatbelt.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/fluent}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
source "${PROJECT_DIR}/tests/lib/work_test_fixtures.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_project() {
  TEST_DIR="$(mktemp -d "${LOG_DIR}/case.XXXXXX")"
  PROJECT="${TEST_DIR}/project"
  ISOLATED_HOME="${TEST_DIR}/isolated-home"
  OPERATOR_HOME="${TEST_DIR}/operator-home"
  EXTERNAL_BIN="${OPERATOR_HOME}/.local/bin"
  PACKAGE_ROOT="${OPERATOR_HOME}/.local/lib/node_modules/@openai/codex"
  CODEX_HOME_FIXTURE="${TEST_DIR}/codex-home"
  LAUNCH_LOG="${TEST_DIR}/launcher.log"
  ACCESS_LOG="${TEST_DIR}/access.log"
  PROFILE_LOG="${TEST_DIR}/profile.sb"

  mkdir -p "$PROJECT" "$ISOLATED_HOME" "$EXTERNAL_BIN" \
    "${PACKAGE_ROOT}/bin" "${PACKAGE_ROOT}/runtime" "$CODEX_HOME_FIXTURE"
  printf 'secret outside launcher closure\n' > "${OPERATOR_HOME}/secret.txt"
  printf 'fixture authentication\n' > "${CODEX_HOME_FIXTURE}/auth.json"
  printf '{"name":"@openai/codex"}\n' > "${PACKAGE_ROOT}/package.json"

  cd "$PROJECT"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  seed_review_skill_stubs "."
  seed_tester_config "."
  git add .
  git commit -m "init" > /dev/null 2>&1
  "$FLUENT_BIN" work-item create work-1 --title "Codex boundary" > /dev/null
  "$FLUENT_BIN" attempt create work-1 attempt-1 > /dev/null
}

write_external_codex_package() {
  cat > "${PACKAGE_ROOT}/bin/codex" <<'LAUNCHER'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$0" >> "$CODEX_BOUNDARY_LAUNCH_LOG"
if [[ "$*" == *"login status"* ]]; then
  exit 0
fi
target="$(readlink "$0")"
exec "$(dirname "$target")/../runtime/codex-runtime" "$@"
LAUNCHER
  chmod +x "${PACKAGE_ROOT}/bin/codex"

  cat > "${PACKAGE_ROOT}/runtime/codex-runtime" <<'RUNTIME'
#!/usr/bin/env bash
set -euo pipefail
if cat "${CODEX_BOUNDARY_OPERATOR_HOME}/secret.txt" > /dev/null 2>&1; then
  printf 'operator-home-readable\n' >> "$CODEX_BOUNDARY_ACCESS_LOG"
else
  printf 'operator-home-denied\n' >> "$CODEX_BOUNDARY_ACCESS_LOG"
fi
printf 'task output\n' > task-output.txt
git add task-output.txt
git -c commit.gpgsign=false commit -m "Add task output" > /dev/null 2>&1
printf '%s\n' '{"type":"result","subtype":"success","result":"done","session_id":"fixture"}'
RUNTIME
  chmod +x "${PACKAGE_ROOT}/runtime/codex-runtime"
  ln -s "${PACKAGE_ROOT}/bin/codex" "${EXTERNAL_BIN}/codex"
}

configure_sandbox_launcher() {
  if /usr/bin/sandbox-exec -p '(version 1)(allow default)' /usr/bin/true \
    > /dev/null 2>&1; then
    SANDBOX_SUPPORTED=1
    return
  fi

  SANDBOX_SUPPORTED=0
  cat > "${EXTERNAL_BIN}/sandbox-exec" <<'SANDBOX'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == "-f" ]]
cp "$2" "$CODEX_BOUNDARY_PROFILE_LOG"
shift 2
exec "$@"
SANDBOX
  chmod +x "${EXTERNAL_BIN}/sandbox-exec"
}

cleanup_project() {
  cd /
  if [[ -d "${PROJECT}/.git" ]]; then
    git -C "$PROJECT" worktree list --porcelain 2>/dev/null |
      awk '/^worktree / { print $2 }' |
      while read -r worktree; do
        [[ "$worktree" == "$PROJECT" ]] ||
          git -C "$PROJECT" worktree remove --force "$worktree" 2>/dev/null ||
          true
      done
  fi
  rm -rf "$TEST_DIR"
}

test_external_package_launcher_runs_with_isolated_home() {
  setup_project
  write_external_codex_package
  configure_sandbox_launcher

  local result=0
  env -u OPENAI_API_KEY -u CODEX_API_KEY -u CODEX_ACCESS_TOKEN -u CODEX_AUTH_JSON \
    HOME="$ISOLATED_HOME" \
    CODEX_HOME="$CODEX_HOME_FIXTURE" \
    PATH="${EXTERNAL_BIN}:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    CODEX_BOUNDARY_LAUNCH_LOG="$LAUNCH_LOG" \
    CODEX_BOUNDARY_ACCESS_LOG="$ACCESS_LOG" \
    CODEX_BOUNDARY_PROFILE_LOG="$PROFILE_LOG" \
    CODEX_BOUNDARY_OPERATOR_HOME="$OPERATOR_HOME" \
    "$FLUENT_BIN" task run --coder codex \
      work-1 attempt-1 attempt-1-write-1 \
      > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || result=1

  local resolved_launcher
  resolved_launcher="$(cd "$EXTERNAL_BIN" && pwd -P)/codex"
  [[ -s "$LAUNCH_LOG" ]] || result=1
  if [[ -s "$LAUNCH_LOG" ]] &&
    ! awk -v expected="$resolved_launcher" '$0 != expected { exit 1 }' "$LAUNCH_LOG"; then
    printf '    FAIL: readiness and launch did not use %s\n' "$resolved_launcher"
    result=1
  fi
  if ((SANDBOX_SUPPORTED)); then
    [[ "$(cat "$ACCESS_LOG" 2>/dev/null)" == "operator-home-denied" ]] || result=1
  else
    local package_rule operator_rule
    package_rule="(allow file-read*  (subpath \"${PACKAGE_ROOT}\"))"
    operator_rule="(allow file-read*  (subpath \"${OPERATOR_HOME}\"))"
    [[ "$(cat "$PROFILE_LOG")" == *"$package_rule"* ]] || result=1
    [[ "$(cat "$PROFILE_LOG")" != *"$operator_rule"* ]] || result=1
  fi
  [[ "$("$FLUENT_BIN" work-item show work-1 | jq -r '.attempts[0].tasks[0].status')" == "complete" ]] ||
    result=1

  if ((result != 0)); then
    printf '    Stdout:\n%s\n' "$(cat "$TEST_DIR/stdout")"
    printf '    Stderr:\n%s\n' "$(cat "$TEST_DIR/stderr")"
    printf '    Launcher log:\n%s\n' "$(cat "$LAUNCH_LOG" 2>/dev/null || true)"
    printf '    Access log:\n%s\n' "$(cat "$ACCESS_LOG" 2>/dev/null || true)"
  fi
  cleanup_project
  return "$result"
}

test_missing_launcher_pauses_before_coder_launch() {
  setup_project

  local result=0
  if env -u OPENAI_API_KEY -u CODEX_API_KEY -u CODEX_ACCESS_TOKEN -u CODEX_AUTH_JSON \
    HOME="$ISOLATED_HOME" \
    CODEX_HOME="$CODEX_HOME_FIXTURE" \
    PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
    "$FLUENT_BIN" task run --coder codex \
      work-1 attempt-1 attempt-1-write-1 \
      > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    result=1
  fi

  local state
  state="$("$FLUENT_BIN" work-item show work-1)"
  [[ "$(jq -r '.attempts[0].status' <<< "$state")" == "needs-user" ]] || result=1
  [[ "$(jq -r '.attempts[0].tasks[0].started_at' <<< "$state")" == "null" ]] || result=1
  [[ "$(cat "$TEST_DIR/stderr")" == *'cannot resolve `codex` from PATH'* ]] || result=1

  if ((result != 0)); then
    printf '    Stderr:\n%s\n' "$(cat "$TEST_DIR/stderr")"
    printf '    State:\n%s\n' "$state"
  fi
  cleanup_project
  return "$result"
}

test_unsafe_launcher_closure_pauses_before_coder_launch() {
  setup_project
  printf '{"name":"other"}\n' > "${PACKAGE_ROOT}/package.json"
  cat > "${PACKAGE_ROOT}/bin/codex" <<'LAUNCHER'
#!/usr/bin/env sh
exit 0
LAUNCHER
  chmod +x "${PACKAGE_ROOT}/bin/codex"
  ln -s "${PACKAGE_ROOT}/bin/codex" "${EXTERNAL_BIN}/codex"

  local result=0
  if env -u OPENAI_API_KEY -u CODEX_API_KEY -u CODEX_ACCESS_TOKEN -u CODEX_AUTH_JSON \
    HOME="$ISOLATED_HOME" \
    CODEX_HOME="$CODEX_HOME_FIXTURE" \
    PATH="${EXTERNAL_BIN}:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    "$FLUENT_BIN" task run --coder codex \
      work-1 attempt-1 attempt-1-write-1 \
      > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    result=1
  fi

  local state
  state="$("$FLUENT_BIN" work-item show work-1)"
  [[ "$(jq -r '.attempts[0].status' <<< "$state")" == "needs-user" ]] || result=1
  [[ "$(jq -r '.attempts[0].tasks[0].started_at' <<< "$state")" == "null" ]] || result=1
  [[ "$(cat "$TEST_DIR/stderr")" == *"unrecognized Codex package root"* ]] || result=1

  if ((result != 0)); then
    printf '    Stderr:\n%s\n' "$(cat "$TEST_DIR/stderr")"
    printf '    State:\n%s\n' "$state"
  fi
  cleanup_project
  return "$result"
}

run_test "external package launcher runs with isolated HOME" \
  test_external_package_launcher_runs_with_isolated_home
run_test "missing launcher pauses before coder launch" \
  test_missing_launcher_pauses_before_coder_launch
run_test "unsafe launcher closure pauses before coder launch" \
  test_unsafe_launcher_closure_pauses_before_coder_launch
