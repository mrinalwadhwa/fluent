#!/usr/bin/env bash
# Run only from the separately authorized macOS host acceptance stage.
set -euo pipefail
: "${FLUENT_NODE_WORKER_HOST:?separate macOS host execution is required; set FLUENT_NODE_WORKER_HOST=1 in that host stage}"
test "$FLUENT_NODE_WORKER_HOST" = 1

if [[ "$(uname -s)" != Darwin ]]; then
  printf 'error: Node worker enforcement requires macOS Seatbelt\n' >&2
  exit 1
fi
candidate="$(git rev-parse HEAD)"
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"
output="$(pwd -P)/tests/output/node-worker-host/${candidate}-$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir -p "$output"
printf '%s\n' "$candidate" > "$output/candidate.txt"
printf 'candidate=%s\nevidence=%s\n' "$candidate" "$output"

# The same tests use a deterministic launcher in Writer checks. Host acceptance
# must retain the production profile and apply real Seatbelt with denial probes.
unset FLUENT_TEST_HOST_SANDBOX_PREFLIGHT
unset FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC
export FLUENT_NODE_WORKER_HOST=1
set +e
cargo test --offline --features test-support --test node_worker_upgrade \
  > "$output/node-worker.log" 2>&1
result=$?
set -e
printf '%s\n' "$result" > "$output/exit-code.txt"
tail -n 60 "$output/node-worker.log"
test "$result" -eq 0
test "$(git rev-parse HEAD)" = "$candidate"
git diff --quiet
git diff --cached --quiet
printf 'ok tests/host/check-node-worker-upgrade.sh\n'
