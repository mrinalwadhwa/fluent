"""Prepare isolated native fixtures and retain candidate-bound prerequisite proof.

The host entry point requires explicit authorization. Importing this module has
no effects; hermetic Rust regressions exercise the same evidence and setup code.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit

SSH_SCENARIOS = ('sources', 'embedded-observe', 'embedded-control', 'fullscreen',
                 'disconnect', 'completion', 'cancellation', 'failure-return')


def write_json(path, value):
    with path.open('x') as out:
        json.dump(value, out, indent=2)
        out.write('\n')


def wait_command(release):
    return (f'until test -f {shlex.quote(str(release))}; do sleep 1; done; '
            "printf 'native-acceptance-released\\n'")


def fixture_instructions(provider, release):
    if provider == 'claude':
        hold = ('Use AskUserQuestion to ask when this invocation may finish, with options '
                'Continue waiting and Complete fixture. Wait for the actual answer. '
                'The operator will use Other to supply an acceptance token and COMPLETE. '
                'If the answer lacks COMPLETE, ask again. Do not give a final response '
                'before that answer. Do not use a standalone sleep. ')
        finish = 'After the operator answers COMPLETE'
    else:
        hold = (f'Use the shell tool to wait for this condition: `{wait_command(release)}`. '
                'The operator owns the release file; do not create it yourself. '
                'Remain in this invocation until the condition completes and accept operator steering. ')
        finish = 'After release'
    return (f'This disposable {provider} fixture exercises native interaction. ' + hold + finish +
            ', create native-result.txt containing '
            f'`native-{provider}` followed by a newline, commit it, then give a final response. '
            'Do not run other providers or change any other tracked files.\n')


def pending_interaction(records, provider, generation, release):
    events = [r.get('event', {}) for r in records if r.get('execution_generation') == generation]
    if provider == 'claude':
        return any(e.get('hook_event_name') == 'PreToolUse' and e.get('tool_name') == 'AskUserQuestion' for e in events)
    return any(e.get('type') == 'tool_execution_start' and
               shlex.quote(str(release)) in json.dumps(e.get('args', {})) for e in events)


def assess_invocation(records, provider, generation, marker, release):
    records = [r for r in records if r.get('format') == 'fluent-native-agent-event'
               and r.get('provider') == provider and r.get('execution_generation') == generation]
    events = [r.get('event', {}) for r in records]
    result = {'state': 'incomplete', 'generation': generation, 'diagnostic': 'No completed native invocation; inspect transcript.jsonl and task logs.'}
    for e in events:
        message = e.get('message', {})
        if e.get('type') == 'message_end' and message.get('stopReason') == 'error':
            # Point at original bytes rather than republishing arbitrary server error text.
            if 'Unexpected message role' in message.get('errorMessage', ''):
                result['diagnostic'] = ('Pi message_end records a server role rejection. Inspect the original '
                    'errorMessage in transcript.jsonl and the selected local model. If the server rejects '
                    'developer, test compat.supportsDeveloperRole=false in the private fixture config; '
                    'retain the original failure and rerun the real native invocation. No acceptance granted.')
            else:
                result['diagnostic'] = ('Pi message_end records a provider error. Inspect its original errorMessage '
                    'in transcript.jsonl; verify the selected endpoint, model, API and credentials. No acceptance granted.')
            return result
    if not records or not generation:
        return result
    conversations = {r.get('conversation_id') for r in records}
    if len(conversations) != 1 or not all(r.get('conversation_id') and r.get('conversation_id') == r.get('assigned_conversation_id') for r in records):
        result['diagnostic'] = 'Native evidence does not belong to one assigned conversation.'
        return result
    waiting = pending_interaction(records, provider, generation, release)
    if provider == 'claude':
        questions = {e.get('tool_use_id') for e in events if e.get('hook_event_name') == 'PreToolUse'
                     and e.get('tool_name') == 'AskUserQuestion' and e.get('tool_use_id')}
        answers = [i for i, e in enumerate(events) if e.get('hook_event_name') == 'PostToolUse'
                   and e.get('tool_name') == 'AskUserQuestion' and e.get('tool_use_id') in questions
                   and marker and marker in json.dumps(e.get('tool_response', {}))
                   and 'COMPLETE' in json.dumps(e.get('tool_response', {}))]
        admitted = bool(answers)
        stops = [i for i, e in enumerate(events) if e.get('hook_event_name') == 'Stop']
        completed = admitted and bool(stops) and min(stops) > min(answers)
        activity = waiting
    else:
        admitted = bool(marker) and any(r.get('input_sequence', 0) > 1
                    and r['event'].get('type') == 'input' and marker in r['event'].get('text', '') for r in records)
        activity = any(e.get('type') == 'agent_start' for e in events)
        outcomes = [e.get('outcome') for e in events if e.get('type') == 'agent_before_settle']
        completed = bool(outcomes) and all(o == 'completed' for o in outcomes) and any(
            e.get('type') == 'agent_settled' and e.get('aborted') is False for e in events)
    if admitted and waiting and activity and completed:
        result.update(state='pass', diagnostic='Assigned native input, condition wait, activity and settlement retained.')
    else:
        result['diagnostic'] = (f'Incomplete native evidence: admitted operator input={admitted}, '
                                f'condition wait={waiting}, activity={activity}, settlement={completed}. '
                                'Inspect transcript.jsonl, terminal.typescript and task exit status.')
    return result


def prepare_dashboard_home(home, sources):
    home.mkdir(mode=0o700)
    config = home / '.config/fluent'
    config.mkdir(parents=True)
    write_json(config / 'dashboard-sources.json', {'schema_version': 1, 'ssh': sources})
    return home


def dashboard_environment(environment, home):
    return dict(environment, HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
                CFFIXED_USER_HOME=str(home))


def installed_entry(executable, provider):
    entry = Path(executable).resolve(strict=True)
    # pnpm's cmd-shim records the real entry point; never execute a shim to
    # discover it or copy its entire installation/configuration directory.
    with entry.open('rb') as source:
        header = source.read(16384)
    if header.startswith(b'#!/bin/sh'):
        targets = re.findall(rb'^# cmd-shim-target=(.+)$', header, re.MULTILINE)
        if len(targets) != 1:
            raise RuntimeError(f'Unsupported installed {provider} shell shim: {entry}; select a native executable or package bin')
        entry = Path(os.fsdecode(targets[0])).resolve(strict=True)
    for parent in entry.parents:
        manifest = parent / 'package.json'
        if manifest.is_file():
            package = json.loads(manifest.read_text())
            bins = package.get('bin', {})
            target = bins.get(provider) if isinstance(bins, dict) else bins
            if target and (parent / target).resolve() == entry:
                return entry, parent
    with entry.open('rb') as source:
        if source.read(2) == b'#!':
            raise RuntimeError(f'Cannot resolve installed {provider} package resources for {entry}')
    return entry, None


def copy_runtime_file(source, target):
    shutil.copyfile(source, target)
    # Do not copy macOS immutable/restricted flags from signed system binaries.
    Path(target).chmod(Path(source).stat().st_mode & 0o777)
    return target


def copy_node_runtime(source, target):
    """Copy one installed executable and its directly linked libnode runtime."""
    copy_runtime_file(source, target)
    links = subprocess.check_output(['otool', '-L', str(source)], text=True)
    libraries = []
    for name in re.findall(r'^\s+@rpath/(libnode[^\s/]+\.dylib)\s', links, re.MULTILINE):
        original = source.parent.parent / 'lib' / name
        if not original.is_file():
            raise RuntimeError(f'Installed Node runtime is missing {original}')
        private = target.parent.parent / 'lib' / name
        private.parent.mkdir(parents=True, exist_ok=True)
        copy_runtime_file(original, private)
        libraries.append(private)
    return libraries


def copy_installed_package(source, destination, packages):
    source = source.resolve(strict=True)
    if source in packages:
        return packages[source]
    target = destination / str(len(packages))
    packages[source] = target
    # Package manager links must terminate in the private copy. Only declared
    # dependencies may leave a package; arbitrary resource links fail closed.
    def ignore(directory, names):
        return set(names) & {'node_modules', '.git', '.npmrc'}
    links = []
    for directory, dirs, files in os.walk(source):
        dirs[:] = [name for name in dirs if name not in ignore(directory, dirs)]
        for name in dirs + [name for name in files if name not in ignore(directory, files)]:
            original = Path(directory) / name
            if original.is_symlink():
                resolved = original.resolve(strict=True)
                if not resolved.is_relative_to(source):
                    raise RuntimeError(f'Installed resource outside its package: {original}')
                links.append((original.relative_to(source), resolved.relative_to(source)))
    shutil.copytree(source, target, symlinks=True, ignore=ignore, copy_function=copy_runtime_file)
    for relative, resolved in links:
        link = target / relative
        link.unlink()
        link.symlink_to(os.path.relpath(target / resolved, link.parent))
    manifest = json.loads((source / 'package.json').read_text())
    optional = manifest.get('optionalDependencies', {})
    dependencies = {**manifest.get('dependencies', {}), **manifest.get('peerDependencies', {}), **optional}
    for name in dependencies:
        if not re.fullmatch(r'(?:@[A-Za-z0-9_.-]+/)?[A-Za-z0-9_.-]+', name) or name in ('.', '..'):
            raise RuntimeError(f'Invalid installed package dependency: {name}')
        dependency = next((parent / 'node_modules' / name for parent in (source, *source.parents)
                           if (parent / 'node_modules' / name / 'package.json').is_file()), None)
        if dependency is None:
            if name in optional or manifest.get('peerDependenciesMeta', {}).get(name, {}).get('optional'):
                continue
            raise RuntimeError(f'Missing installed dependency {name} required by {source}')
        copied = copy_installed_package(dependency, destination, packages)
        link = target / 'node_modules' / name
        link.parent.mkdir(parents=True, exist_ok=True)
        link.symlink_to(os.path.relpath(copied, link.parent), target_is_directory=True)
    return target


def prepare_provider_launches(home, selected, environment):
    # common.sb already admits these private tool paths. Copy code rather than
    # granting access to the operator HOME or symlinking back outside confinement.
    bin_dir = home / '.local/bin'
    bin_dir.mkdir(parents=True)
    package_dir = home / '.npm-packages'
    package_dir.mkdir()
    packages = {}
    node_libraries = []
    # Pi probes its managed terminal with Node's spawn("tmux", ...). A bare
    # subprocess lookup must succeed before reaching operator PATH entries
    # outside the isolated HOME. Fluent's own canonical tmux lookup does not
    # protect provider subprocesses from those denied paths.
    tmux = shutil.which('tmux', path=environment.get('PATH'))
    if not tmux:
        raise RuntimeError('Installed tmux is required for native provider terminals')
    tmux_source = Path(tmux).resolve(strict=True)
    tmux_target = copy_runtime_file(tmux_source, bin_dir / 'tmux')
    tmux_identity = {'source_executable': str(tmux_source), 'launch_executable': str(tmux_target),
                     'sha256': hashlib.sha256(tmux_target.read_bytes()).hexdigest()}
    for provider, metadata in selected.items():
        metadata['tmux_runtime'] = tmux_identity
        if provider == 'codex':
            # Codex's production launcher stages its own runtime.
            continue
        entry, package = installed_entry(metadata['executable'], provider)
        target = bin_dir / provider
        if package is None:
            if sys.platform == 'darwin':
                node_libraries.extend(copy_node_runtime(entry, target))
            else:
                copy_runtime_file(entry, target)
        else:
            copied = copy_installed_package(package, package_dir, packages)
            target.symlink_to(os.path.relpath(copied / entry.relative_to(package), bin_dir))
            if not (bin_dir / 'node').exists():
                node = shutil.which('node', path=environment.get('PATH'))
                if not node:
                    raise RuntimeError('Installed Node runtime is required for native provider packages')
                node_source = Path(node).resolve(strict=True)
                if sys.platform == 'darwin':
                    node_libraries.extend(copy_node_runtime(node_source, bin_dir / 'node'))
                else:
                    copy_runtime_file(node_source, bin_dir / 'node')
        metadata['launch_executable'] = str(target)
        metadata['entry_sha256'] = hashlib.sha256(target.read_bytes()).hexdigest()
    launch = dashboard_environment(environment, home)
    launch['PATH'] = str(bin_dir) + os.pathsep + environment.get('PATH', os.defpath)
    if node_libraries:
        launch['DYLD_LIBRARY_PATH'] = str(bin_dir.parent / 'lib')
    launch['CLAUDE_CONFIG_DIR'] = str(home / '.claude')
    launch['PI_CODING_AGENT_DIR'] = str(home / '.pi/agent')
    for key in ('NODE_PATH', 'NODE_OPTIONS'):
        launch.pop(key, None)
    return launch


def settle_owned_task(child, binary, project, work, run):
    # Fluent owns worker groups. Never discover or signal them by process name,
    # a persisted PID, or the operator's global tmux server.
    run([str(binary), 'attempt', 'cancel', work, 'attempt-1'], cwd=project, timeout=60)
    if child is not None:
        child.wait(timeout=30)


def finish_prerequisites(output, candidate, results, cleanup_ok):
    missing = [p for p in ('claude', 'pi') if results.get(p, {}).get('state') != 'pass']
    result = {'candidate': candidate, 'scope': 'native-prerequisites-only', 'providers': results,
              'cleanup': cleanup_ok, 'ssh_acceptance': 'pending'}
    if missing or not cleanup_ok:
        # Exclusive names preserve every earlier incomplete observation.
        write_json(output / f'incomplete{len(list(output.glob("incomplete*.json"))) or ""}.json', result)
        raise RuntimeError('Incomplete prerequisites: ' + ', '.join(missing + ([] if cleanup_ok else ['cleanup'])))
    write_json(output / 'result.json', result)


def require_ssh_journeys(observations):
    for provider in ('codex', 'claude', 'pi'):
        for scenario in SSH_SCENARIOS:
            result = observations.get(provider, {}).get(scenario, '')
            if not result.startswith('pass ') or len(result) <= 10:
                raise RuntimeError(f'Incomplete SSH journey: {provider} / {scenario}')


def read_events(path, live=False):
    if not path.exists():
        return []
    lines = path.read_text().splitlines()
    records = []
    for i, line in enumerate(lines):
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError:
            # The writer may be in the middle of the final append while polling.
            if not live or i != len(lines) - 1:
                raise
    return records


def prepare_pi_config(source, model, endpoint, developer_role, private):
    selected = copy.deepcopy(source['providers']['local-openai'])
    models = [m for m in selected['models'] if m['id'] == model]
    if len(models) != 1 or selected.get('api') != 'openai-completions':
        raise RuntimeError('Select exactly one local-openai model using openai-completions')
    url = urlsplit(endpoint)
    if (url.scheme != 'http' or url.hostname not in ('127.0.0.1', 'localhost') or not url.port
            or url.username or url.password or url.query or url.fragment):
        raise RuntimeError('Pi prerequisite endpoint must be localhost HTTP with an explicit port and no credentials')
    selected['models'] = models
    selected['baseUrl'] = endpoint
    if developer_role is not None:
        if developer_role not in ('true', 'false'):
            raise RuntimeError('FLUENT_NATIVE_PI_SUPPORTS_DEVELOPER_ROLE must be true or false')
        models[0].setdefault('compat', {})['supportsDeveloperRole'] = developer_role == 'true'
    write_json(private / 'models.json', {'providers': {'local-openai': selected}})
    (private / 'models.json').chmod(0o600)
    # The private file may contain an operator credential; never copy it to evidence.
    return {'model': model, 'api': selected['api'], 'endpoint': endpoint,
            'supportsDeveloperRole': models[0].get('compat', {}).get('supportsDeveloperRole',
                selected.get('compat', {}).get('supportsDeveloperRole', 'auto'))}


class Commands:
    def __init__(self, output, environment):
        self.output = output
        self.environment = environment
        self.sequence = 0

    def run(self, args, cwd=None, timeout=60, interactive=False):
        self.sequence += 1
        prefix = self.output / f'command-{self.sequence:04}'
        write_json(prefix.with_suffix('.json'), {'argv': [str(a) for a in args], 'cwd': str(cwd) if cwd else None})
        with prefix.with_suffix('.log').open('xb') as out:
            if interactive:
                # Own the script/attachment transport as a fresh process group.
                # On interruption, signal only this live child-owned group.
                child = subprocess.Popen(args, cwd=cwd, env=self.environment, start_new_session=True)
                try:
                    status = child.wait(timeout=timeout)
                except BaseException:
                    if child.poll() is None:
                        try:
                            os.killpg(child.pid, signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                        try:
                            child.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            os.killpg(child.pid, signal.SIGKILL)
                            child.wait(timeout=5)
                    raise
                finally:
                    prefix.with_suffix('.status').write_text(str(child.poll()) + '\n')
                result = subprocess.CompletedProcess(args, status)
            else:
                result = subprocess.run(args, cwd=cwd, env=self.environment, stdout=out,
                                        stderr=subprocess.STDOUT, timeout=timeout)
                prefix.with_suffix('.status').write_text(str(result.returncode) + '\n')
        if result.returncode and not interactive:
            raise RuntimeError(f'Command failed ({result.returncode}); inspect {prefix.with_suffix(".log")}')
        return prefix.with_suffix('.log').read_text()


def prepare_project(project, binary, provider, model, effort, commands, source=None, candidate=None):
    if source is None:
        project.mkdir()
        commands.run(['git', 'init', '-b', 'main'], cwd=project)
        (project / 'README.md').write_text('Disposable native acceptance fixture.\n')
        (project / '.gitignore').write_text('.fluent/*\n!.fluent/tester.yaml\n!.fluent/extract-tester-results\n')
        (project / '.fluent').mkdir()
        (project / '.fluent/tester.yaml').write_text('commands:\n  - command: test -f README.md\n    test_harness: shell-harness\n')
        (project / '.fluent/extract-tester-results').write_text(
            '#!/bin/sh\nprintf \'[]\\n\'\n')
        (project / '.fluent/extract-tester-results').chmod(0o755)
    else:
        # SSH's canonical gate requires the exact candidate checkout. Do not
        # replace its Tester contract with the small prerequisite fixture's.
        commands.run(['git', 'clone', '--no-hardlinks', str(source), str(project)], timeout=120)
        commands.run(['git', 'checkout', '--detach', candidate], cwd=project)
    commands.run(['git', 'config', 'user.name', 'Native Acceptance'], cwd=project)
    commands.run(['git', 'config', 'user.email', 'native-fixture@example.com'], cwd=project)
    commands.run(['git', 'config', 'commit.gpgsign', 'false'], cwd=project)
    if source is None:
        commands.run(['git', 'add', '.'], cwd=project)
        commands.run(['git', 'commit', '-m', 'Add native fixture', '-m', '- Exercise native interaction in a disposable project.'], cwd=project)
    # Explicit project config prevents the operator's layered Fluent preferences
    # from selecting providers or changing the fixture's terminal mode.
    (project / '.fluent/config.yaml').write_text('terminal:\n  mode: native\n')
    work = 'native-' + provider
    artifact_root = project / '.fluent/work/artifacts'
    for work_id in (work, work + '-cancel') if source is not None else (work,):
        release = artifact_root / work_id / 'attempt-1/attempt-1-write-1/operator-release'
        commands.run([str(binary), 'work-item', 'create', work_id, '--title', 'Native acceptance fixture',
                      '--instructions', fixture_instructions(provider, release), '--learner-mode', 'no-expertise'], cwd=project)
        commands.run([str(binary), 'attempt', 'create', work_id, 'attempt-1', '--terminal-mode', 'native',
                      '--coder', provider, '--model', model, '--effort', effort], cwd=project)
    return work, artifact_root / work / 'attempt-1/attempt-1-write-1/operator-release'


def check_clean(root, commands, candidate=None):
    head = commands.run(['git', 'rev-parse', 'HEAD'], cwd=root).strip()
    if candidate and head != candidate:
        raise RuntimeError('Candidate changed during acceptance')
    if commands.run(['git', 'status', '--porcelain'], cwd=root).strip():
        raise RuntimeError('Acceptance requires a clean candidate')
    return head


def capture_startup_terminal(project, work, commands):
    # Read only this disposable Task's published binding. Never discover a
    # socket by process name or address the operator's default tmux server.
    artifact = project / '.fluent/work/artifacts' / work / 'attempt-1/attempt-1-write-1'
    binding = project / '.fluent/work/terminal-sessions' / work / 'attempt-1/attempt-1-write-1/terminal-session.json'
    try:
        stored = json.loads(binding.read_text())
        descriptor = stored['descriptor']
        if (descriptor['work_item_id'] != work or descriptor['attempt_id'] != 'attempt-1'
                or descriptor['task_id'] != 'attempt-1-write-1'
                or Path(stored['execution_record']) != artifact / 'execution.json'
                or not stored.get('socket')):
            return
        # Commands retains original bytes, argv and exit status even if the
        # provider dies during capture. These are diagnostics, never pass proof.
        commands.run(['tmux', '-S', stored['socket'], 'capture-pane', '-p', '-S', '-', '-t', 'fluent:0.0'], timeout=5)
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.TimeoutExpired):
        # A terminal can settle before publication or between binding and read.
        # Preserve Task/provider diagnostics even when no pane remains.
        return


def exercise_provider(output, binary, provider, model, effort, commands):
    project = output / provider
    work, release = prepare_project(project, binary, provider, model, effort, commands)
    artifact = release.parent
    transcript = artifact / 'transcript.jsonl'
    child = None
    result = {'state': 'incomplete', 'diagnostic': 'Native invocation was not exercised.'}
    cleanup_ok = False
    try:
        with (output / f'{provider}-task.log').open('xb') as log:
            argv = [str(binary), 'task', 'run', work, 'attempt-1', 'attempt-1-write-1']
            write_json(output / f'{provider}-task-command.json', {'argv': argv, 'cwd': str(project)})
            child = subprocess.Popen(argv,
                                     cwd=project, env=commands.environment, stdout=log, stderr=subprocess.STDOUT)
            deadline = time.monotonic() + 600
            descriptor = None
            while time.monotonic() < deadline:
                capture_startup_terminal(project, work, commands)
                if child.poll() is not None:
                    raise RuntimeError('Native Task ended before interaction; inspect task log, transcript.jsonl and capture-pane command logs')
                terminals = json.loads(commands.run([str(binary), 'terminal', 'list', '--source', str(project)]))
                matches = [t for t in terminals if t['work_item_id'] == work and t['task_id'] == 'attempt-1-write-1' and t['can_control']]
                if len(matches) == 1 and pending_interaction(read_events(transcript, live=True), provider, matches[0]['execution_generation'], release):
                    descriptor = matches[0]
                    break
                time.sleep(1)
            if descriptor is None:
                raise RuntimeError('No admitted live native invocation within 600 seconds')
            write_json(output / f'{provider}-invocation.json', descriptor)
            generation = descriptor['execution_generation']
            marker = 'native-operator-' + os.urandom(12).hex()
            if provider == 'claude':
                action = f'Answer the pending question using Other: {marker} COMPLETE'
            else:
                action = f'Send: Please acknowledge {marker} and continue waiting for the operator release.'
            print(f'\n{provider}: {action}\n'
                  'Detach with Ctrl-b then d after the provider accepts the input. '
                  'The harness will then observe settlement.', flush=True)
            commands.run(['/usr/bin/script', '-q', str(output / f'{provider}-terminal.typescript'), str(binary),
                          'terminal', 'attach', '--source', str(project), descriptor['session_id'], '--control'], timeout=600, interactive=True)
            if provider != 'claude':
                admitted = [r for r in read_events(transcript) if r.get('execution_generation') == generation
                            and r.get('input_sequence', 0) > 1 and r.get('event', {}).get('type') == 'input']
                if not any(marker in r['event'].get('text', '') for r in admitted) or child.poll() is not None:
                    raise RuntimeError('Operator input did not reach the still-live assigned native invocation')
                release.touch(exist_ok=False)
            status = child.wait(timeout=600)
            (output / f'{provider}-task.status').write_text(str(status) + '\n')
            result = assess_invocation(read_events(transcript), provider, generation, marker, release)
            if status != 0:
                result['state'] = 'incomplete'
                result['diagnostic'] += f' Task command exited {status}; inspect {provider}-task.log.'
    except BaseException as error:
        records = read_events(transcript)
        generation = next((r.get('execution_generation') for r in records if r.get('execution_generation')), '')
        result = assess_invocation(records, provider, generation, '', release)
        result['state'] = 'incomplete'
        result['diagnostic'] += ' ' + str(error)
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            raise
    finally:
        capture_startup_terminal(project, work, commands)
        try:
            settle_owned_task(child, binary, project, work, commands.run)
            terminals = json.loads(commands.run([str(binary), 'terminal', 'list', '--source', str(project)]))
            cleanup_ok = not any(t['work_item_id'] == work and (t['can_control'] or t['can_observe']) for t in terminals)
            if not cleanup_ok:
                result['diagnostic'] += ' Owned terminal remains accessible after cancellation.'
        except Exception as error:
            result['diagnostic'] += ' Cleanup failed: ' + str(error)
        if child is not None:
            (output / f'{provider}-process-status.txt').write_text(str(child.poll()) + '\n')
        write_json(output / f'{provider}-observation.json', result)
    return result, cleanup_ok


def without_credentials(value):
    if isinstance(value, dict):
        return {k: without_credentials(v) for k, v in value.items()
                if k.lower() not in ('apikey', 'headers', 'authorization')}
    if isinstance(value, list):
        return [without_credentials(v) for v in value]
    return value


def prepare_ssh(output, root, binary, candidate, selected, source_config, environment, commands):
    # Only the credential-free fixture configuration is durable. Authenticated
    # endpoints require FLUENT_NATIVE_PI_API_KEY in the launching host shell.
    config = output / 'pi-config'
    config.mkdir(mode=0o700)
    pi = prepare_pi_config(without_credentials(source_config), selected['pi']['model'],
                           environment['FLUENT_NATIVE_PI_ENDPOINT'],
                           environment.get('FLUENT_NATIVE_PI_SUPPORTS_DEVELOPER_ROLE'), config)
    write_json(output / 'selected-pi.json', pi)
    sources = []
    for provider in ('codex', 'claude', 'pi'):
        host = environment[f'FLUENT_SSH_{provider.upper()}_HOST']
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.@-]*', host):
            raise RuntimeError('Use an existing pinned OpenSSH alias')
        resolved = dict(line.split(' ', 1) for line in commands.run(['/usr/bin/ssh', '-G', host]).splitlines() if ' ' in line)
        if resolved.get('hostname') not in ('localhost', '127.0.0.1', '::1'):
            raise RuntimeError('Disposable SSH preparation is restricted to localhost')
        project = output / provider
        work, _ = prepare_project(project, binary, provider, selected[provider]['model'], selected[provider]['effort'], commands, root, candidate)
        sources.append({'name': 'native-' + provider, 'host': host, 'project_root': str(project)})
        launch_env = ['env'] + [f'{key}={commands.environment[key]}' for key in ('HOME', 'XDG_CONFIG_HOME', 'CFFIXED_USER_HOME', 'CODEX_HOME', 'PATH')]
        launch_env += [f'PI_CODING_AGENT_DIR={config}', f'FLUENT_NATIVE_PI_ENDPOINT={pi["endpoint"]}']
        # These scripts contain paths and public model selection only. Existing
        # host credentials remain in the inherited environment or keychain.
        for suffix, work_id in [('', work), ('-cancel', work + '-cancel')]:
            for operation, argv in [('start', ['task', 'run', work_id, 'attempt-1', 'attempt-1-write-1']),
                                    ('cancel', ['attempt', 'cancel', work_id, 'attempt-1'])]:
                script = output / f'{operation}-{provider}{suffix}.sh'
                script.write_text('#!/usr/bin/env bash\nset -euo pipefail\nset -o noclobber\ncd ' + shlex.quote(str(project)) + '\n' +
                                  shlex.join(launch_env + [str(binary)] + argv) +
                                  ' > ' + shlex.quote(str(output / f'{operation}-{provider}{suffix}.log')) + ' 2>&1\n')
    denied = environment['FLUENT_SSH_DENIED_HOST']
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.@-]*', denied):
        raise RuntimeError('Use an existing localhost alias for the failed-source scenario')
    resolved = dict(line.split(' ', 1) for line in commands.run(['/usr/bin/ssh', '-G', denied]).splitlines() if ' ' in line)
    if resolved.get('hostname') not in ('localhost', '127.0.0.1', '::1'):
        raise RuntimeError('Failed-source fixture must also use localhost')
    sources.append({'name': 'native-denied', 'host': denied, 'project_root': str(output / 'codex')})
    dashboard = prepare_dashboard_home(output / 'dashboard-home', sources)
    (output / 'bin').mkdir()
    (output / 'bin/fluent').symlink_to(binary)
    write_json(output / 'prepared.json', {'candidate': candidate, 'dashboard_home': str(dashboard), 'sources': sources,
               'state': 'prepared-only', 'binary': str(binary), 'pi_config': str(config), 'ssh_acceptance': 'pending'})
    print(f'Prepared only: {output}. Use start-<provider>.sh and cancel-<provider>.sh for these owned Attempts. '
          f'Dashboard HOME: {dashboard}. Configure the existing pinned localhost SSH service to resolve '
          f'{output / "bin/fluent"} without replacing the installed Fluent. No workers or transports were started.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prepare-ssh', action='store_true', help='prepare exact-candidate checkouts and an isolated dashboard; do not launch workers or SSH')
    args = parser.parse_args()
    if os.environ.get('FLUENT_TEST_REAL_NATIVE_AGENT_HOST') != '1' or os.environ.get('FLUENT_NATIVE_DISPOSABLE_FIXTURES') != '1':
        raise RuntimeError('Explicit host/provider and disposable-fixture authorization is required')
    root = Path(__file__).resolve().parents[2]
    output = Path(os.environ['FLUENT_NATIVE_ACCEPTANCE_OUTPUT']).resolve()
    if output == root or root in output.parents:
        raise RuntimeError('Evidence must be outside the candidate checkout')
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    results = {p: {'state': 'pending'} for p in ('claude', 'pi')}
    candidate = ''
    cleanup_ok = True
    try:
        environment = dict(os.environ)
        # Reject synthetic provider seams. These checks are for the installed CLIs.
        if any(k.startswith('FLUENT_TEST_') and k != 'FLUENT_TEST_REAL_NATIVE_AGENT_HOST' for k in environment):
            raise RuntimeError('Remove synthetic FLUENT_TEST_* settings before host acceptance')
        if os.uname().sysname != 'Darwin' or (not args.prepare_ssh and not (os.isatty(0) and os.isatty(1))):
            raise RuntimeError('A macOS host and interactive terminal are required')
        commands = Commands(output, environment)
        candidate = check_clean(root, commands)
        (output / 'candidate.txt').write_text(candidate + '\n')
        (output / 'command.txt').write_text('bash tests/host/check-native-acceptance-prerequisites.sh\n' if not args.prepare_ssh else 'python3 -I tests/fixtures/native-acceptance.py --prepare-ssh\n')
        # Build in a dedicated directory with no test-support feature and never install.
        commands.run(['bash', 'scripts/prepare-terminal-deps.sh'], cwd=root, timeout=600)
        commands.run(['cargo', 'build', '--offline', '--bin', 'fluent', '--target-dir', str(output / 'build')], cwd=root, timeout=1200)
        binary = output / 'build/debug/fluent'
        version = commands.run([str(binary), '--version'])
        if candidate[:8] not in version:
            raise RuntimeError('Production binary does not identify the exact candidate')
        write_json(output / 'binary.json', {'candidate': candidate, 'version': version,
                   'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'path': str(binary)})
        providers = ('codex', 'claude', 'pi') if args.prepare_ssh else ('claude', 'pi')
        selected = {}
        for provider in providers:
            executable = shutil.which(provider)
            if not executable or 'provider-doubles' in executable:
                raise RuntimeError(f'Installed native {provider} CLI is required')
            version = commands.run([executable, '--version'])
            selected[provider] = {'model': environment[f'FLUENT_NATIVE_{provider.upper()}_MODEL'],
                                  'effort': environment[f'FLUENT_NATIVE_{provider.upper()}_EFFORT'],
                                  'version': version, 'executable': executable}
        write_json(output / 'providers.json', selected)
        (output / 'host.txt').write_text(commands.run(['sw_vers']) + commands.run(['tmux', '-V']))
        original_home = Path(environment['HOME'])
        pi_source = Path(environment.get('PI_CODING_AGENT_DIR', str(original_home / '.pi/agent'))) / 'models.json'
        source_config = json.loads(pi_source.read_text())
        home = prepare_dashboard_home(output / 'fluent-home', [])
        commands.environment = prepare_provider_launches(home, selected, environment)
        # Check the exact staged launch paths after HOME/PATH change, retaining
        # original stdout/stderr and status before any Task starts.
        write_json(output / 'provider-launches.json', selected)
        for provider in providers:
            version = commands.run([provider, '--version'])
            if version != selected[provider]['version']:
                raise RuntimeError(f'Staged {provider} version differs from the installed provider')
        commands.environment['CODEX_HOME'] = environment.get('CODEX_HOME', str(original_home / '.codex'))
        if args.prepare_ssh:
            prepare_ssh(output, root, binary, candidate, selected, source_config, environment, commands)
        else:
            # Private configuration may contain credentials; it is outside the
            # evidence directory and is removed only after worker settlement.
            with tempfile.TemporaryDirectory(prefix='fluent-native-acceptance-') as private_dir:
                private = Path(private_dir)
                pi = prepare_pi_config(source_config, selected['pi']['model'], environment['FLUENT_NATIVE_PI_ENDPOINT'],
                                       environment.get('FLUENT_NATIVE_PI_SUPPORTS_DEVELOPER_ROLE'), private)
                write_json(output / 'selected-pi.json', pi)
                commands.environment['PI_CODING_AGENT_DIR'] = str(private)
                for provider in providers:
                    result, cleaned = exercise_provider(output, binary, provider, selected[provider]['model'], selected[provider]['effort'], commands)
                    results[provider] = result
                    cleanup_ok = cleanup_ok and cleaned
                    if not cleaned:
                        break
        check_clean(root, commands, candidate)
        if not args.prepare_ssh:
            finish_prerequisites(output, candidate, results, cleanup_ok)
    except BaseException as error:
        write_json(output / 'failure.json', {'candidate': candidate, 'diagnostic': str(error), 'providers': results, 'acceptance': 'incomplete'})
        raise


if __name__ == '__main__':
    main()
