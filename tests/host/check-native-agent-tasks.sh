#!/usr/bin/env bash
set -euo pipefail

candidate="$(git rev-parse HEAD)"
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"
printf 'candidate=%s\n' "$candidate"
printf '%s\n' 'plan: disposable native Codex Writer, Claude Writer, and Pi Writer'
printf '%s\n' 'plan: queued input, detachment, evidence, cancellation, and ephemeral Codex review'

: "${FLUENT_TEST_REAL_NATIVE_AGENT_HOST:?host provider-run authorization is required}"
: "${FLUENT_NATIVE_CODEX_MODEL:?resolved Codex model is required}"
: "${FLUENT_NATIVE_CODEX_EFFORT:?resolved Codex effort is required}"
: "${FLUENT_NATIVE_CLAUDE_MODEL:?resolved Claude model is required}"
: "${FLUENT_NATIVE_CLAUDE_EFFORT:?resolved Claude effort is required}"
: "${FLUENT_NATIVE_PI_MODEL:?resolved Pi local-openai model is required}"
: "${FLUENT_NATIVE_PI_EFFORT:?resolved Pi thinking level is required}"
: "${FLUENT_NATIVE_PI_ENDPOINT:?Pi local-openai endpoint is required}"
test "$FLUENT_TEST_REAL_NATIVE_AGENT_HOST" = 1

printf 'codex=%s\n' "$(codex --version)"
printf 'claude=%s\n' "$(claude --version)"
printf 'pi=%s\n' "$(pi --version)"
printf 'codex-model=%s codex-effort=%s\n' "$FLUENT_NATIVE_CODEX_MODEL" "$FLUENT_NATIVE_CODEX_EFFORT"
printf 'claude-model=%s claude-effort=%s\n' "$FLUENT_NATIVE_CLAUDE_MODEL" "$FLUENT_NATIVE_CLAUDE_EFFORT"
printf 'pi-model=%s pi-endpoint=%s pi-thinking=%s\n' "$FLUENT_NATIVE_PI_MODEL" "$FLUENT_NATIVE_PI_ENDPOINT" "$FLUENT_NATIVE_PI_EFFORT"
printf 'tmux=%s\n' "$(tmux -V)"
printf 'host=%s\n' "$(sw_vers -productVersion)"

unset FLUENT_TEST_HOST_SANDBOX_PREFLIGHT
unset FLUENT_TEST_HERMETIC_PROVIDERS
unset FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC
cargo nextest run --offline --features test-support --test native_agent_tasks \
  --no-tests fail --run-ignored all --test-threads 1 --success-output immediate \
  -E 'test(host_native_agent_acceptance)'
test "$(git rev-parse HEAD)" = "$candidate"
