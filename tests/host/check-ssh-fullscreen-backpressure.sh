#!/usr/bin/env bash
# Authorized host proof for the controlled PTY, with hermetic SSH/tmux clients.
# Real SSH/native-provider acceptance remains check-ssh-agent-terminal.sh.
set -euo pipefail
: "${FLUENT_SSH_BACKPRESSURE_HOST:?explicit host-check authorization is required}"
: "${FLUENT_SSH_BACKPRESSURE_OUTPUT:?choose a new evidence directory outside the candidate checkout}"
test "$FLUENT_SSH_BACKPRESSURE_HOST" = 1
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
test -z "$(git status --porcelain)"
output=$(python3 -I -S - "$FLUENT_SSH_BACKPRESSURE_OUTPUT" "$root" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1]).resolve()
assert not path.is_relative_to(Path(sys.argv[2]).resolve()), "evidence must be outside the candidate"
path.mkdir(parents=True, exist_ok=False)
(path / "pty").mkdir()
print(path)
PY
)
export FLUENT_SSH_PTY_EVIDENCE_DIR="$output/pty"
cargo test --offline --features test-support --test ssh_agent_sources --no-run --message-format=json > "$output/build.jsonl" 2> "$output/build.stderr"
test_binary=$(python3 -I -S - "$output" <<'PY'
import hashlib, json, subprocess, sys
from pathlib import Path
output = Path(sys.argv[1])
artifacts = [json.loads(line) for line in (output / 'build.jsonl').read_text().splitlines()]
def executable(name):
    matches = [Path(a['executable']).resolve() for a in artifacts
               if a.get('reason') == 'compiler-artifact' and a.get('executable')
               and a['target']['name'] == name]
    assert len(matches) == 1, (name, matches)
    return matches[0]
test = executable('ssh_agent_sources')
helper = executable('fluent')
receipt = {
    'candidate': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
    'command': 'bash tests/host/check-ssh-fullscreen-backpressure.sh',
    'cwd': str(Path.cwd()),
    'transport': 'hermetic OpenSSH/tmux clients; production full-screen loop and SSH helper',
    'real_ssh_native_provider_journey': 'separately required; not established by this check',
    'binaries': [{'path': str(p), 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in (test, helper)],
}
(output / 'candidate.json').write_text(json.dumps(receipt, indent=2) + '\n')
print(test)
PY
)
result=0
"$test_binary" ssh_fullscreen --nocapture > "$output/harness.stdout" 2> "$output/harness.stderr" || result=$?
printf '%s\n' "$result" > "$output/exit-code"
if [[ "$result" != 0 ]]; then
    printf 'not ok SSH full-screen backpressure; evidence: %s\n' "$output"
    exit "$result"
fi
# Require evidence for the long stall and each lifecycle path in both modes.
python3 -I -S - "$output" <<'PY'
import json, sys
from pathlib import Path
output = Path(sys.argv[1])
for mode in ('observe', 'control'):
    for scenario in ('drain', 'detach', 'finish', 'cancel', 'disconnect'):
        name = f'{mode}-{scenario}-9'
        receipt = json.loads((output / 'pty' / (name + '.json')).read_text())
        assert receipt['wait_status'] == 0 and receipt['stall_seconds'] == 9
        capture = (output / 'pty' / (name + '.pty')).read_bytes()
        assert b'SSH_FULLSCREEN_RETURNED' in capture
assert 'ssh_fullscreen_stalled_display_remains_bounded ... ok' in (output / 'harness.stdout').read_text()
PY
printf 'ok SSH full-screen backpressure; evidence: %s\n' "$output"
