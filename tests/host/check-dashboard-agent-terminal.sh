#!/usr/bin/env bash
# Record operator-driven acceptance against packaged, identified binaries.
# This is a host check, never part of the unattended Writer/Tester suite.
set -euo pipefail
: "${FLUENT_TEST_REAL_DASHBOARD_HOST:?separate host and provider-run authorization is required}"
test "$FLUENT_TEST_REAL_DASHBOARD_HOST" = 1
: "${FLUENT_DASHBOARD_HOST_OUTPUT:?choose a new evidence directory outside the checkout}"
: "${FLUENT_DASHBOARD_CODEX_SOURCE:?provide an authorized disposable native Codex source}"
: "${FLUENT_DASHBOARD_CLAUDE_SOURCE:?provide an authorized disposable native Claude source}"
: "${FLUENT_DASHBOARD_PI_SOURCE:?provide an authorized disposable native Pi source}"
test -t 0 && test -t 1
test "$(uname -s)" = Darwin
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
git diff --quiet
git diff --cached --quiet
test -z "$(git ls-files --others --exclude-standard)"
candidate=$(git rev-parse HEAD)
output=$FLUENT_DASHBOARD_HOST_OUTPUT
test ! -e "$output"
mkdir -p "$output"
output=$(cd "$output" && pwd)
case "$output/" in "$root/"*) echo 'Evidence must be outside the candidate checkout.' >&2; exit 1;; esac
printf '%s\n' "$candidate" > "$output/candidate.txt"
{
  sw_vers
  rustc -vV
  tmux -V
  codex --version
  claude --version
  pi --version
} > "$output/versions.txt" 2>&1
cp tools/terminal-deps.env tools/ghostty-LICENSE "$output/"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  bash scripts/prepare-terminal-deps.sh "$target" > "$output/prepare-$target.log" 2>&1
  cargo build --offline --release --target "$target" > "$output/build-$target.log" 2>&1
  binary="$output/fluent-$target"
  cp "target/$target/release/fluent" "$binary"
  codesign --sign - --force "$binary" > "$output/sign-$target.log" 2>&1
  codesign --verify --strict "$binary" >> "$output/sign-$target.log" 2>&1
  case "$target" in aarch64-*) arch=arm64;; x86_64-*) arch=x86_64;; esac
  test "$(lipo -archs "$binary")" = "$arch"
  shasum -a 256 "$binary" > "$binary.sha256"
  cp "target/terminal-deps/install/$target/fluent-inputs" "$output/inputs-$target.txt"
done
host=$(rustc -vV | sed -n 's/^host: //p')
binary="$output/fluent-$host"
test -x "$binary"

cat > "$output/scenarios.txt" <<'SCENARIOS'
Observe: t keeps the project list visible; identity names the actual provider,
Task and generation. A second observer cannot type or resize the worker.
Control: i visibly acquires Control; type and paste Unicode (界 é) and multiline
text. Esc and Ctrl-C reach the provider without canceling the Attempt. Ctrl-]
returns to navigation; ] sends the literal reserved chord. A competing client
cannot acquire Control. Confirm only the selected invocation receives input.
Resize: resize while controlling; confirm wrapping, cursor, Unicode width,
alternate-screen content, and native interaction after repeated changes.
Handoff: Ctrl-] then f; detach with the native tmux binding. Confirm selection,
mouse preference, raw mode, paste, and dimensions after return. Repeat in
Observe. End the worker during full-screen attachment and repeat after an
attachment failure; the dashboard must remain usable on every return path.
End/reconnect: disconnect and t reconnect the same generation; inspect a full
repaint. End it, start a later invocation, and confirm the old view stays Ended
and cannot type. Select the later invocation explicitly to regain access.
Flood/isolation: request sustained output, navigate and refresh Work while it
arrives, and inspect memory. Emit title/clipboard/escape sequences in the test
worker; they must remain inert outside its embedded pane. Reconnect and verify
the repaint. Retain before/after process-memory samples with the capture.
Nested: repeat the journey inside an outer tmux. Use the outer send-prefix
binding before inner detach; verify Esc/Ctrl-C and the dashboard escape chord
route to their intended owners. Exit the nested dashboard and outer shell.
SCENARIOS

for provider in codex claude pi; do
  case "$provider" in
    codex) source=$FLUENT_DASHBOARD_CODEX_SOURCE;;
    claude) source=$FLUENT_DASHBOARD_CLAUDE_SOURCE;;
    pi) source=$FLUENT_DASHBOARD_PI_SOURCE;;
  esac
  "$binary" terminal list --source "$source" > "$output/$provider-terminals.json"
  python3 - "$output/$provider-terminals.json" "$provider" <<'PY'
import json, sys
rows = json.load(open(sys.argv[1]))
assert any(r['provider'] == sys.argv[2] and r['can_control'] for r in rows), 'no live native provider terminal'
PY
  for mode in direct nested; do
    printf '\nProvider: %s; mode: %s; candidate: %s\n' "$provider" "$mode" "$candidate"
    cat "$output/scenarios.txt"
    printf '\nComplete the scenarios; q exits the dashboard to record each result.\n'
    if [[ "$mode" == direct ]]; then
      /usr/bin/script -q "$output/$provider-$mode.typescript" "$binary" dashboard "$source"
    else
      # A dedicated outer server avoids changing the operator's tmux config.
      outer="$output/$provider-outer.socket"
      /usr/bin/script -q "$output/$provider-$mode.typescript" tmux -S "$outer" -f /dev/null new-session "$binary" dashboard "$source"
      tmux -S "$outer" kill-server 2>/dev/null || true
    fi
    for scenario in observe control resize handoff end-reconnect flood-isolation; do
      printf '%s / %s / %s: enter pass followed by concrete observations, or fail: ' "$provider" "$mode" "$scenario"
      IFS= read -r result
      printf '%s\n' "$result" > "$output/$provider-$mode-$scenario.txt"
      case "$result" in 'pass '?*) ;; *) echo 'Host acceptance incomplete.' >&2; exit 1;; esac
    done
    "$binary" terminal list --source "$source" > "$output/$provider-$mode-after.json"
  done
done
test "$(git rev-parse HEAD)" = "$candidate"
git diff --quiet
git diff --cached --quiet
printf 'Recorded operator attestations for %s. Targeted Tests Review must inspect captures and observations.\n' "$candidate" | tee "$output/result.txt"
