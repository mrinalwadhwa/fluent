"""Record candidate-bound, operator-driven acceptance on existing SSH hosts."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess


def run(args):
    return subprocess.check_output(args, stderr=subprocess.STDOUT, timeout=30).decode()


def remote(host, args):
    # These are fixed verbs with quoted operands chosen by the operator. No
    # remote response supplies a command or a local filesystem authority.
    return run([
        '/usr/bin/ssh', '-T', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
        '-o', 'ForwardAgent=no', '-o', 'ClearAllForwardings=yes', '-o', 'ForwardX11=no',
        '-o', 'PermitLocalCommand=no', '-o', 'ConnectTimeout=5', '--', host,
        shlex.join(args),
    ])


def clean():
    run(['git', 'diff', '--exit-code'])
    run(['git', 'diff', '--cached', '--exit-code'])
    assert not run(['git', 'ls-files', '--others', '--exclude-standard']).strip(), 'candidate must be clean'


root = Path.cwd().resolve()
clean()
candidate = run(['git', 'rev-parse', 'HEAD']).strip()
output = Path(os.environ['FLUENT_SSH_HOST_OUTPUT']).resolve()
assert output != root and root not in output.parents, 'evidence must be outside the candidate'
output.mkdir(parents=True, exist_ok=False)
(output / 'candidate.txt').write_text(candidate + '\n')
(output / 'command.txt').write_text('bash tests/host/check-ssh-agent-terminal.sh\n')
binary = Path(os.environ['FLUENT_SSH_LOCAL_BIN']).resolve(strict=True)
local_project = Path(os.environ['FLUENT_SSH_LOCAL_PROJECT']).resolve(strict=True)
local_version = run([str(binary), '--version'])
assert candidate[:8] in local_version, 'local production binary must identify the candidate'
(output / 'local-version.txt').write_text(local_version)
(output / 'local-binary.sha256').write_text(hashlib.sha256(binary.read_bytes()).hexdigest() + '\n')
(output / 'local-host.txt').write_text(run(['sw_vers']) + run(['tmux', '-V']))
config_path = Path.home() / '.config/fluent/dashboard-sources.json'
config = json.loads(config_path.read_text())
assert config['schema_version'] == 1
configured = {s['name']: s for s in config['ssh']}
assert len(configured) == len(config['ssh']), 'duplicate source names'
scenarios = {
    'sources': 'Show local and remote projects together with distinct source/host identities. Navigate local Work while another configured source fails authentication or host verification; record the source-specific remedy.',
    'embedded-observe': 'Select the recorded remote Task/generation and observe its native interface. Attempt typing and resizing from Observe; verify no worker input or size change.',
    'embedded-control': 'Acquire Control, enter Unicode and multiline text, resize, use Esc and Ctrl-C, then Ctrl-] to navigate. Prove only the chosen invocation receives input and a competing attachment cannot obtain Control. Preserve its actual responses and peer observation.',
    'fullscreen': 'Use f in both Observe and Control. Show the same Task/generation and retained exclusive Control, resize while controlling, use native tmux detach, and verify dashboard input, selection and terminal modes on return. Repeat within an outer tmux.',
    'disconnect': 'Queue recognizable input while interrupting this SSH connection. Confirm Disconnected and that the remote worker continues. Reconnect with t: prove the same generation, Observe mode, no replay, and explicit i to regain Control. Retain before/after owner identities.',
    'completion': 'Let the assigned native invocation finish while attached. Prove the workflow advances, ordinary Task evidence remains authoritative, and the ended terminal rejects later input. Start a later disposable invocation through normal Fluent controls; reconnecting the old view must report unavailable and must not restart or retarget it.',
    'cancellation': 'Attach to a separately prepared disposable native invocation, cancel its Attempt through the normal remote Fluent command, and verify child settlement, attachment invalidation and authoritative cancellation evidence. Keep the local companion project usable.',
    'failure-return': 'Exercise attachment failure and worker completion during full-screen access. Verify dashboard restoration after each. Keep provider output and concrete action/result observations.',
}
(output / 'scenarios.json').write_text(json.dumps(scenarios, indent=2) + '\n')
try:
    for provider in ['codex', 'claude', 'pi']:
        name = os.environ['FLUENT_SSH_' + provider.upper() + '_SOURCE']
        source = configured[name]
        host = source['host']
        project = source['project_root']
        assert host and not host.startswith('-') and all(c.isascii() and (c.isalnum() or c in '._-@') for c in host)
        assert project.startswith('/') and not any(ord(c) < 32 for c in project)
        (output / (provider + '-source.json')).write_text(json.dumps(source, indent=2) + '\n')
        assert remote(host, ['uname', '-s']).strip() == 'Darwin', 'remote macOS is required'
        remote_head = remote(host, ['git', '-C', project, 'rev-parse', 'HEAD']).strip()
        assert remote_head == candidate, 'remote fixture checkout must contain the exact candidate'
        remote(host, ['git', '-C', project, 'diff', '--exit-code'])
        remote(host, ['git', '-C', project, 'diff', '--cached', '--exit-code'])
        assert not remote(host, ['git', '-C', project, 'ls-files', '--others', '--exclude-standard']).strip(), 'remote candidate must be clean'
        version = remote(host, ['fluent', '--version'])
        assert candidate[:8] in version, 'remote installed binary must identify this candidate'
        versions = remote(host, ['sw_vers']) + version + remote(host, ['tmux', '-V']) + remote(host, [provider, '--version'])
        (output / (provider + '-remote-versions.txt')).write_text(versions)
        # The fixed remote command hashes the existing installed executable;
        # its reported path is never executed or opened on the local host.
        digest = remote(host, ['sh', '-c', 'shasum -a 256 "$(command -v fluent)"'])
        (output / (provider + '-remote-binary.sha256')).write_text(digest)
        before = remote(host, ['fluent', 'agent-source', 'snapshot', '--project-root', project])
        (output / (provider + '-before.json')).write_text(before)
        snapshot = json.loads(before)
        assert snapshot['schema_version'] == 1 and snapshot['source_id'] == project
        terminals = [w['terminal'] for item in snapshot['work'] for w in item['workers'] if w['terminal']]
        matches = [t for t in terminals if t['provider'] == provider and t['can_control']]
        assert matches, provider + ' has no live native terminal: prerequisite remains pending'
        (output / (provider + '-invocations.json')).write_text(json.dumps(matches, indent=2) + '\n')
        print('\nProvider:', provider, 'source:', name, 'host:', host, 'candidate:', candidate, flush=True)
        print(json.dumps(matches, indent=2), flush=True)
        for scenario, description in scenarios.items():
            print(scenario + ': ' + description, flush=True)
        print('Complete these journeys in the dashboard, then q to record observations. Use an authorized second terminal to interrupt SSH or cancel fixture Work.', flush=True)
        capture = output / (provider + '.typescript')
        subprocess.run(['/usr/bin/script', '-q', str(capture), str(binary), 'dashboard', str(local_project)], check=True)
        for scenario in scenarios:
            result = input(provider + ' / ' + scenario + ': enter pass followed by concrete action/result observations and evidence references, or fail: ')
            (output / (provider + '-' + scenario + '.txt')).write_text(result + '\n')
            assert result.startswith('pass ') and len(result) > 10, 'host acceptance remains incomplete'
        after = remote(host, ['fluent', 'agent-source', 'snapshot', '--project-root', project])
        (output / (provider + '-after.json')).write_text(after)
        (output / (provider + '-remote-status.txt')).write_text(remote(host, ['fluent', 'status', project]))
        assert remote(host, ['git', '-C', project, 'rev-parse', 'HEAD']).strip() == candidate
    clean()
    assert run(['git', 'rev-parse', 'HEAD']).strip() == candidate
    (output / 'result.txt').write_text('Recorded all provider journeys. Targeted Tests Review must inspect captures and action/result evidence before accepting this required check.\n')
except BaseException as error:
    (output / 'incomplete.txt').write_text(str(error) + '\n')
    raise
