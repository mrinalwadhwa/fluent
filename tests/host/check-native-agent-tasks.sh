#!/usr/bin/env bash
set -euo pipefail

candidate="$(git rev-parse HEAD)"
candidate_root="$(pwd -P)"
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"
printf 'candidate=%s\n' "$candidate"
printf '%s\n' 'plan: disposable native Codex, Claude, and Pi Writers with attached steering and follow-up input'
printf '%s\n' 'plan: observe, detach, reconnect, inspect canonical evidence, then cancel one Task per provider'
printf '%s\n' 'plan: hold a Codex observer through automatic Review, then inspect native Learner and disposable conflict Rebase'
printf '%s\n' 'plan: Pi transient-response retry, idle compaction with queued continuation, abort, and receiver loss'

: "${FLUENT_TEST_REAL_NATIVE_AGENT_HOST:?host provider-run authorization is required}"
: "${FLUENT_NATIVE_CODEX_MODEL:?resolved Codex model is required}"
: "${FLUENT_NATIVE_CODEX_EFFORT:?resolved Codex effort is required}"
: "${FLUENT_NATIVE_CLAUDE_MODEL:?resolved Claude model is required}"
: "${FLUENT_NATIVE_CLAUDE_EFFORT:?resolved Claude effort is required}"
: "${FLUENT_NATIVE_PI_MODEL:?resolved Pi local-openai model is required}"
: "${FLUENT_NATIVE_PI_EFFORT:?resolved Pi thinking level is required}"
: "${FLUENT_NATIVE_PI_ENDPOINT:?Pi local-openai endpoint is required}"
test "$FLUENT_TEST_REAL_NATIVE_AGENT_HOST" = 1
python3 - <<'PY'
import os
import socket
import json
from pathlib import Path
from urllib.parse import urlparse

endpoint = urlparse(os.environ["FLUENT_NATIVE_PI_ENDPOINT"])
if endpoint.scheme != "http" or endpoint.hostname not in {"localhost", "127.0.0.1"} or not endpoint.port:
    raise SystemExit("Pi endpoint must be a local HTTP server with an explicit port")
with socket.create_connection((endpoint.hostname, endpoint.port), timeout=3):
    pass
source = Path(os.environ.get("PI_CODING_AGENT_DIR", str(Path.home() / ".pi/agent"))) / "models.json"
config = json.loads(source.read_text())
provider = config["providers"]["local-openai"]
if provider.get("api") != "openai-completions":
    raise SystemExit("selected Pi provider must use openai-completions")
selected = [entry for entry in provider["models"] if entry.get("id") == os.environ["FLUENT_NATIVE_PI_MODEL"]]
if len(selected) != 1:
    raise SystemExit("selected Pi model must appear exactly once in local-openai models.json")
print("pi-selected-model=" + selected[0]["id"])
print("pi-selected-fields=" + ",".join(sorted(selected[0].keys())))
PY
claude_version="$(claude --version)"
python3 - "$claude_version" <<'PY'
import re
import sys

match = re.search(r"\b(\d+)\.(\d+)\.(\d+)\b", sys.argv[1])
if not match or tuple(map(int, match.groups())) < (2, 1, 295):
    raise SystemExit("native Claude requires Code 2.1.295 or later for fail-closed hooks")
PY

printf 'codex=%s\n' "$(codex --version)"
printf 'claude=%s\n' "$claude_version"
printf 'pi=%s\n' "$(pi --version)"
printf 'codex-model=%s codex-effort=%s\n' "$FLUENT_NATIVE_CODEX_MODEL" "$FLUENT_NATIVE_CODEX_EFFORT"
printf 'claude-model=%s claude-effort=%s\n' "$FLUENT_NATIVE_CLAUDE_MODEL" "$FLUENT_NATIVE_CLAUDE_EFFORT"
printf 'pi-model=%s pi-endpoint=%s pi-thinking=%s\n' "$FLUENT_NATIVE_PI_MODEL" "$FLUENT_NATIVE_PI_ENDPOINT" "$FLUENT_NATIVE_PI_EFFORT"
printf 'tmux=%s\n' "$(tmux -V)"
printf 'expect=%s\n' "$(command -v expect)"
printf 'host=%s\n' "$(sw_vers -productVersion)"

host_output="${FLUENT_NATIVE_HOST_OUTPUT_DIR:-$(dirname "$candidate_root")/main/.fluent/work/artifacts/20261011-213742-dashboard-agent-terminals-native-agent-execution/attempt-1/host-native/${candidate}-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
mkdir -p "$host_output"
export FLUENT_NATIVE_HOST_OUTPUT_DIR="$host_output"
printf 'host-evidence=%s\n' "$host_output"
printf '%s\n' "$candidate" > "$host_output/candidate.txt"
{
  codex --version
  claude --version
  pi --version
  tmux -V
} > "$host_output/provider-versions.txt" 2>&1

set +e
cargo nextest run --offline --features test-support --test terminal_sessions \
  --no-tests fail --run-ignored only --success-output immediate \
  -E 'test(managed_terminal_pane_starts_in_assigned_workspace)' \
  > "$host_output/terminal-cwd.log" 2>&1
terminal_status=$?
set -e
printf 'terminal-cwd-status=%s\n' "$terminal_status" | tee "$host_output/terminal-cwd-status.txt"
tail -n 30 "$host_output/terminal-cwd.log"
test "$terminal_status" -eq 0

# Start the installed app-server and make credential-free initialize and
# config/read calls to check its effective settings before paid model work.
export FLUENT_TEST_INSTALLED_CODEX="$(command -v codex)"
set +e
cargo nextest run --offline --features test-support --lib \
  --no-tests fail --run-ignored only --success-output immediate \
  -E 'test(installed_codex_starts_with_exact_daemon_grants)' \
  > "$host_output/codex-daemon-startup.log" 2>&1
codex_startup_status=$?
set -e
printf 'codex-daemon-startup-status=%s\n' "$codex_startup_status" | tee "$host_output/codex-daemon-startup-status.txt"
tail -n 30 "$host_output/codex-daemon-startup.log"
test "$codex_startup_status" -eq 0

cargo nextest run --offline --features test-support --test native_agent_tasks \
  --no-tests fail --success-output immediate \
  -E 'test(pi_installed_runner_compacts_before_queued_continuation) | test(pi_installed_runner_closes_input_after_receiver_failure)' \
  > "$host_output/pi-installed-runtime.log" 2>&1

unset FLUENT_TEST_HOST_SANDBOX_PREFLIGHT
unset FLUENT_TEST_HERMETIC_PROVIDERS
unset FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC
set +e
cargo nextest run --offline --features test-support --test native_agent_tasks \
  --no-tests fail --run-ignored all --test-threads 1 --success-output immediate \
  -E 'test(host_native_agent_acceptance) | test(host_native_attached_attempt_progression)' \
  > "$host_output/host-test.log" 2>&1
host_status=$?
set -e
printf 'host-test-status=%s\n' "$host_status" | tee "$host_output/status.txt"
tail -n 30 "$host_output/host-test.log"
if [ -f target/debug/fluent ]; then
  shasum -a 256 target/debug/fluent > "$host_output/fluent-binary.sha256"
fi
test "$(git rev-parse HEAD)" = "$candidate"
exit "$host_status"
