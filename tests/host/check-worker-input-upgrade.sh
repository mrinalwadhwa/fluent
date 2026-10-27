#!/usr/bin/env bash
set -euo pipefail

# Run only from the trusted host. A parent sandbox denial is a failure here;
# deterministic provider doubles do not substitute for kernel confinement.
candidate="$(git rev-parse HEAD)"
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"
test "$(uname -s)" = Darwin
output="${FLUENT_WORKER_INPUT_OUTPUT_DIR:-target/worker-input-host/$candidate}"
mkdir -p "$output"
printf '%s\n' "$candidate" > "$output/candidate.txt"
export FLUENT_WORKER_INPUT_REQUIRE_SEATBELT=1
unset FLUENT_BIN_OVERRIDE
set +e
cargo test --offline --features test-support --test worker_input_upgrade -- --nocapture \
  > "$output/results.log" 2>&1
status=$?
set -e
printf '%s\n' "$status" > "$output/status.txt"
tail -n 30 "$output/results.log"
test "$(git rev-parse HEAD)" = "$candidate"
git diff --quiet
git diff --cached --quiet
exit "$status"
