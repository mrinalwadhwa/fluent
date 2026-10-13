#!/usr/bin/env bash
set -euo pipefail

candidate="$(git rev-parse HEAD)"
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"
: "${FLUENT_TEST_REAL_NATIVE_AGENT_HOST:?host provider-run authorization is required}"
: "${FLUENT_NATIVE_CODEX_MODEL:?resolved Codex model is required}"
: "${FLUENT_NATIVE_CODEX_EFFORT:?resolved Codex effort is required}"
test "$FLUENT_TEST_REAL_NATIVE_AGENT_HOST" = 1

output="${FLUENT_NATIVE_HOST_OUTPUT_DIR:-$(dirname "$(pwd -P)")/main/.fluent/work/artifacts/20261011-213742-dashboard-agent-terminals-native-agent-execution/attempt-1/host-native/${candidate}-codex-first-turn-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
mkdir -p "$output"
export FLUENT_NATIVE_HOST_OUTPUT_DIR="$output"
export FLUENT_TEST_NATIVE_CODEX_FIRST_TURN_ONLY=1
printf 'candidate=%s\noutput=%s\n' "$candidate" "$output"
printf '%s\n' "$candidate" > "$output/candidate.txt"
printf 'model=%s\neffort=%s\n' "$FLUENT_NATIVE_CODEX_MODEL" "$FLUENT_NATIVE_CODEX_EFFORT" > "$output/selected-codex.txt"
codex --version > "$output/codex-version.txt"
tmux -V > "$output/tmux-version.txt"

set +e
cargo nextest run --offline --features test-support --test native_agent_tasks \
  --no-tests fail --run-ignored only --success-output immediate \
  -E 'test(host_native_agent_acceptance)' \
  > "$output/codex-first-turn.log" 2>&1
status=$?
set -e
printf '%s\n' "$status" > "$output/status.txt"
tail -n 30 "$output/codex-first-turn.log"
if [ -f target/debug/fluent ]; then
  shasum -a 256 target/debug/fluent > "$output/fluent-binary.sha256"
fi
test "$(git rev-parse HEAD)" = "$candidate"
exit "$status"
