#!/usr/bin/env bash
# Run only as the separately authorized host acceptance step.
set -euo pipefail

: "${FLUENT_CLAUDE_SESSION_HOST:?host provider-run authorization is required}"
: "${FLUENT_CLAUDE_SESSION_MODEL:?resolved Claude model is required}"
: "${FLUENT_CLAUDE_SESSION_EFFORT:?resolved Claude effort is required}"
test "$FLUENT_CLAUDE_SESSION_HOST" = 1
candidate="$(git rev-parse HEAD)"
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"

host_output="${FLUENT_CLAUDE_SESSION_OUTPUT_DIR:-$(pwd -P)/tests/output/claude-session-host-${candidate}-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
mkdir -p "$(dirname "$host_output")"
mkdir "$host_output"
export FLUENT_CLAUDE_SESSION_OUTPUT_DIR="$host_output"
printf '%s\n' "$candidate" > "$host_output/candidate.txt"
{
  claude --version
  sw_vers -productVersion
  printf 'model=%s\neffort=%s\n' "$FLUENT_CLAUDE_SESSION_MODEL" "$FLUENT_CLAUDE_SESSION_EFFORT"
} > "$host_output/environment.txt"
printf 'host-evidence=%s\n' "$host_output"

unset FLUENT_TEST_HERMETIC_PROVIDERS FLUENT_TEST_HOST_SANDBOX_PREFLIGHT
unset FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC FLUENT_BIN_OVERRIDE
case "$(command -v claude)" in
  */tests/fixtures/*) printf 'error: real installed Claude is required\n' >&2; exit 1 ;;
esac
# Keep test-support out of the executable that launches the real provider.
cargo build --offline --bin fluent --target-dir target/claude-session-host \
  > "$host_output/build.log" 2>&1
FLUENT_CLAUDE_SESSION_PRODUCTION_BIN="$(pwd -P)/target/claude-session-host/debug/fluent"
export FLUENT_CLAUDE_SESSION_PRODUCTION_BIN
shasum -a 256 "$FLUENT_CLAUDE_SESSION_PRODUCTION_BIN" > "$host_output/fluent-binary.sha256"
set +e
cargo nextest run --offline --features test-support --test claude_writer_upgrade \
  --run-ignored only --no-tests fail --test-threads 1 --success-output immediate \
  -E 'test(host_claude_writer_session_upgrade)' > "$host_output/host-test.log" 2>&1
host_status=$?
set -e
printf 'host-test-status=%s\n' "$host_status" | tee "$host_output/status.txt"
tail -n 30 "$host_output/host-test.log"
test "$(git rev-parse HEAD)" = "$candidate"
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"
exit "$host_status"
