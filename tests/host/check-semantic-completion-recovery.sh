#!/usr/bin/env bash
set -euo pipefail

# Run only as the separately assigned host check. The fixture provider proves
# coordinator behavior and worker confinement, not native-provider or SSH acceptance.
candidate_root="$(pwd -P)"
candidate="$(git rev-parse HEAD)"
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"
output="${FLUENT_SEMANTIC_COMPLETION_OUTPUT:-$candidate_root/tests/output/semantic-completion-host-$candidate-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
mkdir -p "$output"
output="$(cd "$output" && pwd -P)"
export FLUENT_SEMANTIC_COMPLETION_OUTPUT="$output"
printf 'candidate=%s\nevidence=%s\n' "$candidate" "$output"
printf '%s\n' "$candidate" > "$output/candidate.txt"
printf '%s\n' 'Hermetic provider; production coordinator and actual worker confinement. Native-provider and SSH acceptance remain required.' > "$output/scope.txt"

finish() {
  local result=$?
  trap - EXIT
  printf '%s\n' "$result" > "$output/exit-status.txt"
  if [[ -f tests/output/semantic_completion_recovery_replay_preserves_gates.log ]]; then
    cp tests/output/semantic_completion_recovery_replay_preserves_gates.log "$output/public-commands.log"
  fi
  python3 -I -S - "$output" <<'PY'
import hashlib, json, sys
from pathlib import Path
root = Path(sys.argv[1])
# Only explicitly exported evidence files; never traverse private fixture caches.
files = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
         for path in sorted(root.iterdir()) if path.is_file() and path.name != 'sha256.json'}
(root / 'sha256.json').write_text(json.dumps(files, indent=2) + '\n')
PY
  exit "$result"
}
trap finish EXIT

bash scripts/prepare-terminal-deps.sh > "$output/native-build.log" 2>&1
build_target="$(rustc -vV | sed -n 's/^host: //p')"
export DYLD_LIBRARY_PATH="$candidate_root/target/terminal-deps/install/$build_target/lib${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
# A separate target prevents a feature-unified test binary from becoming the CLI.
cargo build --offline --no-default-features --bin fluent \
  --target-dir target/semantic-completion-production > "$output/production-build.log" 2>&1
export FLUENT_BIN_OVERRIDE="$candidate_root/target/semantic-completion-production/debug/fluent"
shasum -a 256 "$FLUENT_BIN_OVERRIDE" > "$output/production-binary.sha256"
"$FLUENT_BIN_OVERRIDE" version > "$output/production-version.txt"
export FLUENT_SEMANTIC_COMPLETION_HOST=1
unset FLUENT_TEST_HOST_SANDBOX_PREFLIGHT FLUENT_TEST_HERMETIC_PROVIDERS
unset FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC
cargo nextest run --offline --features test-support --test binary \
  --no-tests fail --success-output immediate \
  -E 'test(=semantic_completion_recovery_replay_preserves_gates)' \
  > "$output/host-test.log" 2>&1
# The test must observe confinement; sandbox denial is a failure, never a skip.
test -s "$output/retry-confinement-observed"
test "$(cat "$output/retry-provider-launches")" = launch
test "$(git rev-parse HEAD)" = "$candidate"
git diff --quiet
git diff --cached --quiet
printf '%s\n' 'ok semantic-completion-recovery'
