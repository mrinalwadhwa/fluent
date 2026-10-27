#!/usr/bin/env bash
# Run only through the separately authorized host acceptance boundary.
set -euo pipefail
: "${FLUENT_TEST_REAL_NATIVE_AGENT_HOST:?host provider-run authorization is required}"
test "$FLUENT_TEST_REAL_NATIVE_AGENT_HOST" = 1
test "$(uname -s)" = Darwin
root=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$root"
candidate=$(git rev-parse HEAD)
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"

if [ -n "${FLUENT_NATIVE_HOST_OUTPUT_DIR:-}" ]; then
    output=$FLUENT_NATIVE_HOST_OUTPUT_DIR
    # Never reuse retained logs or an earlier fixture.
    (umask 077; mkdir "$output")
else
    output=$(mktemp -d "${TMPDIR:-/tmp/}fluent-codex-display.XXXXXX")
fi
output=$(cd "$output" && pwd -P)
export FLUENT_NATIVE_HOST_OUTPUT_DIR="$output"
export FLUENT_NATIVE_CODEX_MODEL=gpt-6-astra
export FLUENT_NATIVE_CODEX_EFFORT=low
export FLUENT_TEST_NATIVE_CODEX_DISPLAY_PRESSURE=1
unset FLUENT_TEST_NATIVE_CODEX_FIRST_TURN_ONLY FLUENT_TEST_HERMETIC_PROVIDERS
unset FLUENT_BIN_OVERRIDE FLUENT_TEST_HOST_SANDBOX_PREFLIGHT

printf 'candidate=%s\noutput=%s\n' "$candidate" "$output"
printf '%s\n' "$candidate" > "$output/candidate.txt"
printf 'model=gpt-6-astra\neffort=low\n' > "$output/selected-codex.txt"
printf 'bash tests/host/check-native-codex-display-backpressure.sh\n' > "$output/command.txt"
codex_path=$(command -v codex)
case "$codex_path" in
    *provider-doubles*) echo 'A real installed Codex CLI is required' >&2; exit 1 ;;
esac
printf '%s\n' "$codex_path" > "$output/codex-path.txt"
codex --version > "$output/codex-version.txt"
shasum -a 256 "$codex_path" > "$output/codex-launcher.sha256"
tmux -V > "$output/tmux-version.txt"
command -v expect > "$output/expect-path.txt"

# Build without test-support. The ignored Rust case is only the driver; every
# disposable Work/Attempt/Task and native adapter runs in this production binary.
export FLUENT_PRODUCTION_BIN="$root/target/native-display-host/debug/fluent"
finish() {
    status=$?
    printf '%s\n' "$status" > "$output/status.txt"
}
trap finish EXIT
cargo build --offline --bin fluent --target-dir target/native-display-host \
    > "$output/production-build.log" 2>&1
"$FLUENT_PRODUCTION_BIN" --version > "$output/fluent-version.txt"
shasum -a 256 "$FLUENT_PRODUCTION_BIN" > "$output/fluent-binary.sha256"
cargo nextest run --offline --features test-support --test native_agent_tasks \
    --no-tests fail --run-ignored only --success-output immediate \
    -E 'test(host_native_agent_acceptance)' > "$output/native-display.log" 2>&1
test -s "$output/codex-runtime/display-proof.json"
test -s "$output/codex-runtime/execution-settled.json"
test "$(git rev-parse HEAD)" = "$candidate"
git diff --quiet
git diff --cached --quiet
printf 'ok native Codex display pressure; evidence: %s\n' "$output"
