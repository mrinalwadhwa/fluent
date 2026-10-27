#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../.."
candidate="$(git rev-parse HEAD)"
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"

output="$(pwd -P)/target/completion-repair-host/${candidate}-$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir -p "$output"
printf 'candidate=%s\noutput=%s\n' "$candidate" "$output"
printf '%s\n' "$candidate" > "$output/candidate.txt"
printf '%s\n' 'Hermetic provider; production executable and real worker sandbox.' \
  'This check does not replace real-provider native or SSH acceptance.' \
  > "$output/proof-scope.txt"

# Build separately so the driver cannot accidentally exercise test-support
# admission or substitute the production sandbox launcher.
if ! cargo build --offline --bin fluent --target-dir target/completion-repair-production \
    > "$output/build.log" 2>&1; then
  tail -n 40 "$output/build.log"
  exit 1
fi
production_bin="$(pwd -P)/target/completion-repair-production/debug/fluent"
export FLUENT_BIN_OVERRIDE="$production_bin"
export FLUENT_COMPLETION_REPAIR_PRODUCTION_BIN="$FLUENT_BIN_OVERRIDE"
export FLUENT_COMPLETION_REPAIR_HOST_OUTPUT_DIR="$output"
shasum -a 256 "$FLUENT_BIN_OVERRIDE" > "$output/fluent-binary.sha256"

set +e
cargo test --offline --features test-support --test binary \
  writer_completion_repair_production_boundary -- --ignored --exact --nocapture \
  > "$output/exercise.log" 2>&1
status=$?
set -e
printf '%s\n' "$status" > "$output/status.txt"
tail -n 40 "$output/exercise.log"
test "$(git rev-parse HEAD)" = "$candidate"
git diff --quiet
git diff --cached --quiet
if [ "$status" -eq 0 ]; then
  printf 'ok completion-repair-production\n'
fi
exit "$status"
