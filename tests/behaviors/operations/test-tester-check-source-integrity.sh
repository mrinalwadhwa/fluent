#!/usr/bin/env bash
# test-tester-check-source-integrity — Verify standalone Tester setup is read-only.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/fluent}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

test_tester_check_preserves_project_files() {
  local test_dir
  test_dir="$(mktemp -d -t fluent-test-tester-integrity-XXXXXX)"
  mkdir -p "${test_dir}/.fluent"

  printf 'existing agent instructions\n' > "${test_dir}/AGENTS.md"
  cp "${PROJECT_DIR}/.fluent/extract-tester-results" \
    "${test_dir}/.fluent/extract-tester-results"
  chmod +x "${test_dir}/.fluent/extract-tester-results"
  cat > "${test_dir}/.fluent/tester.yaml" <<'YAML'
commands:
  - command: printf 'ok source-integrity\n'
    test_harness: shell-harness
YAML
  git -C "$test_dir" init -q
  git -C "$test_dir" config user.name test
  git -C "$test_dir" config user.email test@example.invalid
  git -C "$test_dir" add AGENTS.md .fluent
  git -C "$test_dir" commit -qm 'Seed Tester fixture'

  local before_agents output result=0
  before_agents="$(cat "${test_dir}/AGENTS.md")"
  output="$(cd "$test_dir" && "$FLUENT_BIN" --no-sandbox tester check 2>&1)" || result=$?

  if [ "$result" -ne 0 ]; then
    printf '    FAIL: tester check exited %s: %s\n' "$result" "$output"
    result=1
  fi
  if [ "$(cat "${test_dir}/AGENTS.md")" != "$before_agents" ]; then
    printf '    FAIL: tester check changed AGENTS.md\n'
    result=1
  fi
  if [ -e "${test_dir}/.fluent/.gitignore" ]; then
    printf '    FAIL: tester check created .fluent/.gitignore\n'
    result=1
  fi
  if [ -n "$(git -C "$test_dir" status --porcelain=v1 --untracked-files=all)" ]; then
    printf '    FAIL: tester check changed the project worktree\n'
    git -C "$test_dir" status --short
    result=1
  fi

  rm -rf "$test_dir"
  return "$result"
}

printf 'test-tester-check-source-integrity\n\n'

run_test "tester check preserves project files" \
  test_tester_check_preserves_project_files

summarize_and_exit
