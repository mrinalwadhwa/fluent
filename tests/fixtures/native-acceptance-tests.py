"""Hermetic regressions for the separately authorized native host driver."""
import importlib.util
import json
import os
import runpy
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('native_acceptance', Path(__file__).with_name('native-acceptance.py'))
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)


def record(provider, event, sequence=1, generation='assigned'):
    return {'format': 'fluent-native-agent-event', 'schema_version': 1,
            'provider': provider, 'execution_generation': generation,
            'conversation_id': 'session', 'assigned_conversation_id': 'session',
            'input_sequence': sequence, 'event': event}


class NativeAcceptance(unittest.TestCase):
    def test_wait_retains_invocation(self):
        with tempfile.TemporaryDirectory() as root:
            release = Path(root) / "operator's release"
            command = native.wait_command(release)
            child = subprocess.Popen(['/bin/sh', '-c', command], stdout=subprocess.PIPE)
            try:
                with self.assertRaises(subprocess.TimeoutExpired):
                    child.wait(timeout=0.15)
                release.touch()
                self.assertEqual(child.communicate(timeout=3)[0], b'native-acceptance-released\n')
                self.assertEqual(child.returncode, 0)
            finally:
                if child.poll() is None:
                    child.kill()
                child.wait()
            instructions = native.fixture_instructions('claude', release)
            self.assertIn('AskUserQuestion', instructions)
            self.assertIn(command, native.fixture_instructions('pi', release))
            self.assertNotIn('sleep 900', instructions)
            records = [record('claude', {'hook_event_name': 'UserPromptSubmit', 'prompt': 'initial'}),
                       record('claude', {'hook_event_name': 'PreToolUse', 'tool_name': 'AskUserQuestion', 'tool_use_id': 'question-1'}),
                       record('claude', {'hook_event_name': 'PostToolUse', 'tool_name': 'AskUserQuestion',
                                         'tool_use_id': 'question-1', 'tool_response': {'answers': 'operator-token COMPLETE'}}),
                       record('claude', {'hook_event_name': 'Stop'}, 2)]
            self.assertEqual(native.assess_invocation(records, 'claude', 'assigned', 'operator-token', release)['state'], 'pass')
            # A Stop that claims to wait without executing the condition cannot pass.
            self.assertEqual(native.assess_invocation([records[0], records[-1]], 'claude', 'assigned', 'operator-token', release)['state'], 'incomplete')
            # Another generation's input cannot credit this invocation.
            records[2]['execution_generation'] = 'replacement'
            self.assertEqual(native.assess_invocation(records, 'claude', 'assigned', 'operator-token', release)['state'], 'incomplete')

    def test_pi_failure_preserves_diagnosis(self):
        failure = {'role': 'assistant', 'stopReason': 'error', 'errorMessage': '400: Unexpected message role.'}
        records = [record('pi', {'type': 'session_start'}),
                   record('pi', {'type': 'input', 'text': 'initial'}),
                   record('pi', {'type': 'agent_start'}),
                   record('pi', {'type': 'message_end', 'message': failure}),
                   record('pi', {'type': 'agent_before_settle', 'outcome': 'error'}),
                   record('pi', {'type': 'input_entry', 'entry_id': 'admitted'})]
        original = json.dumps(records)
        result = native.assess_invocation(records, 'pi', 'assigned', 'operator-token', Path('/tmp/release'))
        self.assertEqual(result['state'], 'incomplete')
        self.assertIn('supportsDeveloperRole', result['diagnostic'])
        self.assertIn('message_end', result['diagnostic'])
        self.assertEqual(json.dumps(records), original)
        # Even a contradictory successful settlement cannot erase a provider error.
        records.append(record('pi', {'type': 'agent_settled', 'aborted': False}))
        self.assertEqual(native.assess_invocation(records, 'pi', 'assigned', 'operator-token', Path('/tmp/release'))['state'], 'incomplete')
        records[3]['event']['message']['errorMessage'] = 'connection refused'
        result = native.assess_invocation(records, 'pi', 'assigned', 'operator-token', Path('/tmp/release'))
        self.assertNotIn('supportsDeveloperRole', result['diagnostic'])
        self.assertIn('endpoint', result['diagnostic'])

    def test_private_pi_config_preserves_model(self):
        source = {'providers': {'local-openai': {'baseUrl': 'http://127.0.0.1:8000/v1',
                  'api': 'openai-completions', 'apiKey': 'fixture-secret',
                  'models': [{'id': 'chosen', 'reasoning': True, 'maxTokens': 4321,
                              'compat': {'supportsReasoningEffort': False}}, {'id': 'other'}]}}}
        original = json.dumps(source)
        with tempfile.TemporaryDirectory() as root:
            private = Path(root)
            metadata = native.prepare_pi_config(source, 'chosen', 'http://localhost:1234/v1', 'false', private)
            selected = json.loads((private / 'models.json').read_text())['providers']['local-openai']
            self.assertEqual(selected['models'], [{'id': 'chosen', 'reasoning': True, 'maxTokens': 4321,
                'compat': {'supportsReasoningEffort': False, 'supportsDeveloperRole': False}}])
            self.assertEqual(selected['apiKey'], 'fixture-secret')
            self.assertNotIn('fixture-secret', json.dumps(metadata))
            self.assertEqual((private / 'models.json').stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.dumps(source), original)
            for endpoint in ['https://example.com/v1', 'http://127.0.0.1/v1', 'http://key@localhost:80/v1']:
                with self.assertRaisesRegex(RuntimeError, 'localhost HTTP'):
                    native.prepare_pi_config(source, 'chosen', endpoint, None, private)

    def test_disposable_project_keeps_declared_tester(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            output = root / 'logs'
            output.mkdir()
            home = native.prepare_dashboard_home(root / 'home', [])
            commands = native.Commands(output, native.dashboard_environment(os.environ, home))
            actual = commands.run
            calls = []
            def run(args, **kwargs):
                if args[0] == '/fixture/fluent':
                    calls.append(args)
                    return ''
                return actual(args, **kwargs)
            commands.run = run
            project = root / 'project'
            work, release = native.prepare_project(project, '/fixture/fluent', 'claude', 'sonnet', 'low', commands)
            self.assertEqual(work, 'native-claude')
            self.assertIn('AskUserQuestion', calls[0][calls[0].index('--instructions') + 1])
            self.assertFalse(release.exists())
            self.assertEqual(actual(['git', 'status', '--porcelain'], cwd=project), '')
            extractor = actual([str(project / '.fluent/extract-tester-results'), str(root)], cwd=project)
            self.assertIsInstance(json.loads(extractor), list)
            actual(['/bin/sh', '-c', 'test -f README.md'], cwd=project)
            self.assertIn('--terminal-mode', calls[1])

    def test_ssh_preparation_keeps_exact_candidate_and_secrets_out(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            (root / 'logs').mkdir()
            home = native.prepare_dashboard_home(root / 'home', [])
            commands = native.Commands(root / 'logs', native.dashboard_environment(os.environ, home))
            commands.environment['CODEX_HOME'] = str(root / 'operator-codex')
            actual = commands.run
            def run(args, **kwargs):
                if args[:2] == ['/usr/bin/ssh', '-G']:
                    return 'hostname 127.0.0.1\n'
                if args[0] == '/fixture/fluent':
                    return ''
                return actual(args, **kwargs)
            commands.run = run
            source = root / 'source'
            native.prepare_project(source, '/fixture/fluent', 'pi', 'chosen', 'medium', commands)
            candidate = actual(['git', 'rev-parse', 'HEAD'], cwd=source).strip()
            clone = root / 'ssh-fixture'
            native.prepare_project(clone, '/fixture/fluent', 'pi', 'chosen', 'medium', commands, source, candidate)
            self.assertEqual(actual(['git', 'rev-parse', 'HEAD'], cwd=clone).strip(), candidate)
            self.assertEqual(actual(['git', 'status', '--porcelain'], cwd=clone), '')
            self.assertEqual((clone / '.fluent/tester.yaml').read_bytes(), (source / '.fluent/tester.yaml').read_bytes())
            selected = {'providers': {'local-openai': {'api': 'openai-completions', 'apiKey': 'operator-secret', 'headers': {'Authorization': 'Bearer secret'},
                         'models': [{'id': 'chosen', 'headers': {'X-Token': 'secret'}, 'reasoning': True}]}}}
            sanitized = native.without_credentials(selected)
            self.assertNotIn('secret', json.dumps(sanitized))
            self.assertEqual(sanitized['providers']['local-openai']['models'], [{'id': 'chosen', 'reasoning': True}])
            self.assertEqual(selected['providers']['local-openai']['apiKey'], 'operator-secret')
            output = root / 'prepared'
            output.mkdir()
            environment = {'FLUENT_NATIVE_PI_ENDPOINT': 'http://localhost:1234/v1', 'FLUENT_SSH_DENIED_HOST': 'denied-local',
                           **{f'FLUENT_SSH_{p.upper()}_HOST': 'pinned-local' for p in ('codex', 'claude', 'pi')}}
            mapping = {p: {'model': 'chosen', 'effort': 'low'} for p in ('codex', 'claude', 'pi')}
            native.prepare_ssh(output, source, Path('/fixture/fluent'), candidate, mapping, selected, environment, commands)
            prepared = json.loads((output / 'prepared.json').read_text())
            self.assertEqual(prepared['state'], 'prepared-only')
            self.assertEqual(prepared['ssh_acceptance'], 'pending')
            self.assertEqual(len(prepared['sources']), 4)
            self.assertNotIn('operator-secret', (output / 'pi-config/models.json').read_text())
            for provider in ('codex', 'claude', 'pi'):
                for operation in ('start', 'cancel'):
                    script = output / f'{operation}-{provider}.sh'
                    subprocess.run(['bash', '-n', str(script)], check=True)
                    self.assertIn(str(home), script.read_text())
                    self.assertNotIn('operator-secret', script.read_text())
                    cancel_fixture = output / f'{operation}-{provider}-cancel.sh'
                    self.assertIn('native-' + provider + '-cancel', cancel_fixture.read_text())

    def test_provider_launch_survives_isolated_home(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            operator = root / 'operator'
            install = operator / 'Library/pnpm'
            install.mkdir(parents=True)
            package = install / 'store/pi'
            package.mkdir(parents=True)
            (package / 'package.json').write_text(json.dumps({
                'name': 'fixture-pi', 'bin': {'pi': 'cli.js'},
                'dependencies': {'fixture-dep': '1'}}))
            (package / 'resource.txt').write_text('installed resource')
            (package / 'cli.js').write_text(
                '#!/usr/bin/env node\n'
                'const fs = require("fs"); const path = require("path");\n'
                'console.log(fs.readFileSync(path.join(__dirname, "resource.txt"), "utf8"));\n'
                'console.log(require("fixture-dep"));\n'
                'if (process.argv.includes("--fail")) {\n'
                ' console.error("original startup error: missing runtime resource"); process.exit(19); }\n')
            (package / 'cli.js').chmod(0o755)
            dependency = install / 'store/dependency'
            dependency.mkdir()
            (dependency / 'package.json').write_text('{"name":"fixture-dep","main":"index.js"}')
            (dependency / 'index.js').write_text('module.exports = "installed dependency";')
            (package / 'node_modules').mkdir()
            (package / 'node_modules/fixture-dep').symlink_to(dependency, target_is_directory=True)
            # pnpm publishes a shell shim, not a symlink to the entry point.
            pi = install / 'pi'
            pi.write_text('#!/bin/sh\nexec node ' + str(package / 'cli.js') +
                          ' "$@"\n# cmd-shim-target=' + str(package / 'cli.js') + '\n')
            pi.chmod(0o755)
            claude = operator / '.local/bin/claude'
            claude.parent.mkdir(parents=True)
            claude.symlink_to(shutil.which('node'))
            (operator / '.config/fluent').mkdir(parents=True)
            secret = operator / '.config/fluent/config.yaml'
            secret.write_text('operator-private-settings')
            (operator / '.npmrc').write_text('operator-private-credential')
            home = native.prepare_dashboard_home(root / 'fixture', [])
            selected = {p: {'executable': str(exe)} for p, exe in [('claude', claude), ('pi', pi)]}
            environment = dict(os.environ, HOME=str(operator),
                               CLAUDE_CONFIG_DIR=str(operator / '.claude'),
                               PI_CODING_AGENT_DIR=str(operator / '.pi/agent'),
                               NODE_PATH=str(operator / 'injected-modules'),
                               PATH=str(install) + os.pathsep + os.environ['PATH'])
            launch = native.prepare_provider_launches(home, selected, environment)
            # Remove access to every original installation/config path. Launches
            # must use copied code and dependencies, never a source symlink.
            operator.rename(root / 'unavailable-operator')
            logs = root / 'logs'
            logs.mkdir()
            commands = native.Commands(logs, launch)
            self.assertEqual(commands.run(['claude', '--eval', 'console.log("installed native executable")']).strip(),
                             'installed native executable')
            self.assertEqual(commands.run(['pi', '--version']).splitlines(),
                             ['installed resource', 'installed dependency'])
            self.assertEqual(launch['HOME'], str(home))
            self.assertEqual(launch['CLAUDE_CONFIG_DIR'], str(home / '.claude'))
            self.assertEqual(launch['PI_CODING_AGENT_DIR'], str(home / '.pi/agent'))
            self.assertNotIn('NODE_PATH', launch)
            self.assertFalse((home / '.config/fluent/config.yaml').exists())
            self.assertFalse((home / '.npmrc').exists())
            self.assertEqual((root / 'unavailable-operator/.config/fluent/config.yaml').read_text(),
                             'operator-private-settings')
            with self.assertRaisesRegex(RuntimeError, r'Command failed \(19\)'):
                commands.run(['pi', '--fail'])
            self.assertIn('original startup error: missing runtime resource',
                          (logs / 'command-0003.log').read_text())
            self.assertEqual((logs / 'command-0003.status').read_text(), '19\n')

    def test_provider_launch_rejects_external_package_resource(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            package = root / 'package'
            package.mkdir()
            (package / 'package.json').write_text('{"name":"fixture","bin":{"pi":"cli.js"}}')
            (package / 'cli.js').write_text('#!/usr/bin/env node\n')
            (package / 'cli.js').chmod(0o755)
            secret = root / 'operator-auth'
            secret.write_text('must not copy')
            (package / 'resource').symlink_to(secret)
            home = native.prepare_dashboard_home(root / 'fixture', [])
            with self.assertRaisesRegex(RuntimeError, 'outside.*package'):
                native.prepare_provider_launches(home, {'pi': {'executable': str(package / 'cli.js')}}, os.environ)
            self.assertFalse(any(p.is_file() and p.read_bytes() == b'must not copy'
                                 for p in home.rglob('*')))

    def test_node_tmux_subprocess_survives_isolated_home(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(os.environ.get('FLUENT_NATIVE_SUBPROCESS_ROOT', root)).resolve()
            profile = os.environ.get('FLUENT_NATIVE_SUBPROCESS_PROFILE')
            operator = root / 'operator'
            tools = operator / 'bin'
            tools.mkdir(parents=True)
            (operator / 'denied').write_text('operator state')
            for name in ('node', 'tmux'):
                executable = shutil.which(name)
                self.assertIsNotNone(executable, f'{name} is required for the subprocess regression')
                (tools / name).symlink_to(Path(executable).resolve())
            package = operator / 'pi'
            package.mkdir()
            (package / 'package.json').write_text('{"name":"fixture-pi","bin":{"pi":"cli.js"}}')
            (package / 'cli.js').write_text(
                '#!/usr/bin/env node\n'
                'const {spawn} = require("node:child_process");\n'
                'const fs = require("node:fs");\n'
                'if (process.argv[3]) {\n'
                '  try { fs.readFileSync(process.argv[3]); throw new Error("operator state readable"); }\n'
                '  catch (error) { if (!["EPERM", "EACCES"].includes(error.code)) throw error; }\n'
                '  for (const file of [process.argv[3], process.argv[5]]) {\n'
                '    try { fs.writeFileSync(file, "must remain denied"); throw new Error("outside grant writable"); }\n'
                '    catch (error) { if (!["EPERM", "EACCES"].includes(error.code)) throw error; }\n'
                '  }\n'
                '  fs.writeFileSync(process.argv[4], "owned grant remains writable");\n'
                '}\n'
                '// Match Pi 1.3.0 runTmuxShow: spawn a bare tmux, then attach listeners.\n'
                'const child = spawn("tmux", ["-S", process.argv[2], "show", "-gv", "extended-keys"],\n'
                '                    {stdio: ["ignore", "pipe", "pipe"]});\n'
                'child.on("error", error => { console.error(error); process.exitCode = 17; });\n'
                'child.stderr.pipe(process.stderr);\n'
                'child.on("close", code => {\n'
                '  if (code === 1) console.log("tmux query reached the private socket");\n'
                '  else process.exitCode = 18;\n'
                '});\n')
            (package / 'cli.js').chmod(0o755)
            home = native.prepare_dashboard_home(root / 'fixture', [])
            selected = {'pi': {'executable': str(package / 'cli.js')}}
            launch = native.prepare_provider_launches(
                home, selected, dict(os.environ, HOME=str(operator), PATH=str(tools) + os.pathsep + os.defpath))
            # A host test supplies the real production profile and keeps the
            # denied original paths present. The hermetic test instead removes
            # them. Neither test contacts the operator's tmux server.
            if not profile:
                operator.rename(root / 'unavailable-operator')
            logs = root / 'logs'
            logs.mkdir()
            commands = native.Commands(logs, launch)
            argv = ['pi', str(root / 'absent-owned.sock')]
            if profile:
                owner = Path(os.environ['FLUENT_NATIVE_SUBPROCESS_OWNER'])
                argv = ['/usr/bin/sandbox-exec', '-f', profile, str(home / '.local/bin/pi'),
                        str(owner.parent / 'absent-owned.sock'), str(operator / 'denied'),
                        str(owner), os.environ['FLUENT_NATIVE_SUBPROCESS_OUTSIDE']]
            try:
                output = commands.run(argv, cwd=home)
            except RuntimeError:
                self.fail((logs / 'command-0001.log').read_text())
            self.assertIn('tmux query reached the private socket', output)
            self.assertIn('absent-owned.sock', output)
            staged = home / '.local/bin/tmux'
            self.assertTrue(staged.is_file())
            self.assertFalse(staged.is_symlink())
            self.assertEqual(staged.read_bytes(), Path(shutil.which('tmux')).resolve().read_bytes())
            identity = selected['pi']['tmux_runtime']
            self.assertEqual(identity['launch_executable'], str(staged))
            self.assertEqual(identity['sha256'], native.hashlib.sha256(staged.read_bytes()).hexdigest())

    def test_failed_settlement_withholds_cleanup(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            (root / 'logs').mkdir()
            commands = native.Commands(root / 'logs', dict(os.environ))
            fluent = root / 'fluent'
            fluent.write_text('#!/bin/sh\necho "Error: spawn EPERM in runTmuxShow"\nexit 1\n')
            fluent.chmod(0o755)
            release = root / 'pi/artifact/operator-release'
            release.parent.mkdir(parents=True)
            diagnostic = 'terminal pane does not have its generation-specific sandbox grant (owner=1, outside=1)'
            calls = []
            def run(args, **kwargs):
                calls.append(args)
                if args[1:3] == ['attempt', 'cancel']:
                    raise RuntimeError(diagnostic)
                return '[]'
            commands.run = run
            with patch.object(native, 'prepare_project', return_value=('native-pi', release)):
                result, cleaned = native.exercise_provider(root, fluent, 'pi', 'chosen', 'medium', commands)
            self.assertFalse(cleaned)
            self.assertEqual(result['state'], 'incomplete')
            self.assertIn(diagnostic, result['diagnostic'])
            self.assertIn('spawn EPERM', (root / 'pi-task.log').read_text())
            self.assertEqual([args for args in calls if args[1:3] == ['attempt', 'cancel']],
                             [[str(fluent), 'attempt', 'cancel', 'native-pi', 'attempt-1']])
            with self.assertRaises(RuntimeError):
                native.finish_prerequisites(root, 'a' * 40, {'pi': result}, cleaned)
            self.assertFalse(json.loads((root / 'incomplete.json').read_text())['cleanup'])

    def test_startup_capture_keeps_original_diagnostic(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            project = root / 'pi'
            registry = project / '.fluent/work/terminal-sessions/native-pi/attempt-1/attempt-1-write-1'
            registry.mkdir(parents=True)
            artifact = project / '.fluent/work/artifacts/native-pi/attempt-1/attempt-1-write-1'
            artifact.mkdir(parents=True)
            stored = {'descriptor': {'work_item_id': 'native-pi', 'attempt_id': 'attempt-1',
                                     'task_id': 'attempt-1-write-1', 'execution_generation': 'assigned'},
                      'execution_record': str(artifact / 'execution.json'), 'socket': str(root / 'owned.sock')}
            native.write_json(registry / 'terminal-session.json', stored)
            binary = root / 'bin'
            binary.mkdir()
            tmux = binary / 'tmux'
            tmux.write_text('#!/bin/sh\nprintf "original startup ENOENT: provider resource\n"\n')
            tmux.chmod(0o755)
            logs = root / 'logs'
            logs.mkdir()
            commands = native.Commands(logs, dict(os.environ, PATH=str(binary)))
            native.capture_startup_terminal(project, 'native-pi', commands)
            stored['execution_record'] = str(root / 'unrelated/execution.json')
            (registry / 'terminal-session.json').write_text(json.dumps(stored))
            native.capture_startup_terminal(project, 'native-pi', commands)
            captures = list(logs.glob('command-*.log'))
            self.assertEqual(len(captures), 1)
            self.assertEqual(captures[0].read_text(), 'original startup ENOENT: provider resource\n')
            argv = json.loads((logs / 'command-0001.json').read_text())['argv']
            self.assertEqual(argv[:4], ['tmux', '-S', str(root / 'owned.sock'), 'capture-pane'])

    def test_failed_native_task_keeps_original_evidence(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            (root / 'logs').mkdir()
            commands = native.Commands(root / 'logs', dict(os.environ))
            fluent = root / 'fluent'
            fluent.write_text('#!/bin/sh\necho "original native failure"\nexit 9\n')
            fluent.chmod(0o755)
            project = root / 'pi'
            project.mkdir()
            release = project / 'artifact/operator-release'
            release.parent.mkdir()
            records = [record('pi', {'type': 'message_end', 'message': {
                'role': 'assistant', 'stopReason': 'error', 'errorMessage': '400: Unexpected message role.'}})]
            original = json.dumps(records[0]) + '\n'
            (release.parent / 'transcript.jsonl').write_text(original)
            calls = []
            def run(args, **kwargs):
                calls.append(args)
                return '[]'
            commands.run = run
            with patch.object(native, 'prepare_project', return_value=('native-pi', release)):
                result, cleaned = native.exercise_provider(root, fluent, 'pi', 'chosen', 'medium', commands)
            self.assertEqual(result['state'], 'incomplete')
            self.assertIn('supportsDeveloperRole', result['diagnostic'])
            self.assertTrue(cleaned)
            self.assertIn([str(fluent), 'attempt', 'cancel', 'native-pi', 'attempt-1'], calls)
            self.assertEqual((release.parent / 'transcript.jsonl').read_text(), original)
            self.assertIn('original native failure', (root / 'pi-task.log').read_text())
            self.assertEqual((root / 'pi-process-status.txt').read_text(), '9\n')

    def test_interrupted_transport_settles_only_owned_child(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            commands = native.Commands(root, dict(os.environ))
            unrelated = subprocess.Popen(['/bin/sleep', '30'])
            children = []
            spawn = subprocess.Popen
            def capture(*args, **kwargs):
                child = spawn(*args, **kwargs)
                children.append(child)
                return child
            try:
                with patch('subprocess.Popen', side_effect=capture):
                    with self.assertRaises(subprocess.TimeoutExpired):
                        commands.run(['/bin/sleep', '30'], interactive=True, timeout=0.1)
                self.assertEqual(len(children), 1)
                self.assertIsNotNone(children[0].poll())
                self.assertIsNone(unrelated.poll())
                self.assertTrue((root / 'command-0001.status').exists())
            finally:
                unrelated.terminate()
                unrelated.wait()
                for child in children:
                    if child.poll() is None:
                        child.kill()
                    child.wait()

    def test_fixture_isolation(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            operator = root / 'operator'
            (operator / '.config/fluent').mkdir(parents=True)
            (operator / '.config/fluent/projects.json').write_text('operator registry')
            (operator / '.ssh').mkdir()
            (operator / '.ssh/config').write_text('operator SSH configuration')
            sources = [{'name': 'fixture-pi', 'host': 'pinned-local', 'project_root': str(root / 'pi')}]
            home = native.prepare_dashboard_home(root / 'dashboard', sources)
            env = native.dashboard_environment({'HOME': str(operator), 'XDG_CONFIG_HOME': str(operator / '.config')}, home)
            self.assertEqual(env['HOME'], str(home))
            self.assertEqual(env['XDG_CONFIG_HOME'], str(home / '.config'))
            self.assertEqual((operator / '.config/fluent/projects.json').read_text(), 'operator registry')
            self.assertEqual((operator / '.ssh/config').read_text(), 'operator SSH configuration')
            self.assertFalse((home / '.ssh').exists())
            self.assertEqual(json.loads((home / '.config/fluent/dashboard-sources.json').read_text())['ssh'], sources)
            with self.assertRaises(FileExistsError):
                native.prepare_dashboard_home(home, sources)
            self.assertFalse((home / '.config/fluent/projects.json').exists())

            # Cleanup targets the exact public cancellation command; unrelated children survive.
            owned = subprocess.Popen(['/bin/sleep', '30'])
            unrelated = subprocess.Popen(['/bin/sleep', '30'])
            calls = []
            def cancel(args, **kwargs):
                calls.append((args, kwargs.get('cwd')))
                owned.terminate()
            try:
                native.settle_owned_task(owned, '/candidate/fluent', root / 'fixture', 'owned-work', cancel)
                self.assertIsNotNone(owned.poll())
                self.assertIsNone(unrelated.poll())
                self.assertEqual(calls, [(['/candidate/fluent', 'attempt', 'cancel', 'owned-work', 'attempt-1'], root / 'fixture')])
            finally:
                for child in (owned, unrelated):
                    if child.poll() is None:
                        child.kill()
                    child.wait()

    def test_canonical_ssh_isolates_dashboard_and_retains_failure(self):
        journey = Path(__file__).with_name('ssh-host-journey.py').resolve()
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            home = root / 'operator'
            (home / '.config/fluent').mkdir(parents=True)
            sources = [{'name': p, 'host': 'pinned-local', 'project_root': '/fixture/' + p} for p in ('codex', 'claude', 'pi')]
            config = home / '.config/fluent/dashboard-sources.json'
            original = json.dumps({'schema_version': 1, 'ssh': sources})
            config.write_text(original)
            binary = root / 'fluent'
            binary.write_text('candidate binary fixture')
            candidate = 'a' * 40
            env = {'HOME': str(home), 'FLUENT_SSH_LOCAL_BIN': str(binary), 'FLUENT_SSH_LOCAL_PROJECT': str(root),
                   **{f'FLUENT_SSH_{p.upper()}_SOURCE': p for p in ('codex', 'claude', 'pi')}}
            dashboards = []
            def query(args, **kwargs):
                if args[0] == '/usr/bin/ssh':
                    args = native.shlex.split(args[-1])
                    if args[:2] == ['uname', '-s']:
                        return b'Darwin\n'
                    if args[:3] == ['fluent', 'agent-source', 'snapshot']:
                        project = args[-1]
                        provider = project.rsplit('/', 1)[-1]
                        return json.dumps({'schema_version': 1, 'source_id': project, 'work': [
                            {'workers': [{'terminal': {'provider': provider, 'can_control': True}}]}]}).encode()
                if 'rev-parse' in args:
                    return (candidate + '\n').encode()
                if '--version' in args:
                    return ('fluent 0.6.0 ' + candidate[:8] + '\n').encode()
                return b''
            def dashboard(args, **kwargs):
                dashboards.append(kwargs['env'])
                Path(args[2]).write_text('retained terminal capture')
            for complete in (False, True):
                output = root / ('complete' if complete else 'incomplete')
                env['FLUENT_SSH_HOST_OUTPUT'] = str(output)
                answers = iter(['pass concrete action and evidence'] * (24 if complete else 1) + ['fail missing journey'])
                with patch.dict(os.environ, env), patch('subprocess.check_output', side_effect=query), \
                     patch('subprocess.run', side_effect=dashboard), patch('builtins.input', side_effect=lambda _: next(answers)):
                    if complete:
                        runpy.run_path(str(journey))
                        self.assertTrue((output / 'result.txt').exists())
                    else:
                        with self.assertRaises(AssertionError):
                            runpy.run_path(str(journey))
                        self.assertTrue((output / 'incomplete.txt').exists())
                        self.assertFalse((output / 'result.txt').exists())
                        self.assertEqual((output / 'codex.typescript').read_text(), 'retained terminal capture')
                self.assertEqual(config.read_text(), original)
                self.assertEqual(dashboards[-1]['HOME'], str(output / 'dashboard-home'))
                self.assertEqual(json.loads((output / 'dashboard-home/.config/fluent/dashboard-sources.json').read_text())['ssh'], sources)

    def test_requires_completed_journeys(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            original = root / 'original.jsonl'
            original.write_text('retained failed observation\n')
            results = {'claude': {'state': 'pass'}, 'pi': {'state': 'pending'}}
            with self.assertRaisesRegex(RuntimeError, 'pi'):
                native.finish_prerequisites(root, 'a' * 40, results, True)
            self.assertFalse((root / 'result.json').exists())
            self.assertTrue((root / 'incomplete.json').exists())
            self.assertEqual(original.read_text(), 'retained failed observation\n')
            results['pi']['state'] = 'pass'
            with self.assertRaisesRegex(RuntimeError, 'cleanup'):
                native.finish_prerequisites(root, 'a' * 40, results, False)
            native.finish_prerequisites(root, 'a' * 40, results, True)
            result = json.loads((root / 'result.json').read_text())
            self.assertEqual(result['candidate'], 'a' * 40)
            self.assertEqual(result['scope'], 'native-prerequisites-only')
            self.assertTrue((root / 'incomplete.json').exists())
            observations = {provider: {scenario: 'pass concrete action and evidence' for scenario in native.SSH_SCENARIOS}
                            for provider in ('codex', 'claude', 'pi')}
            native.require_ssh_journeys(observations)
            for provider in observations:
                for scenario in native.SSH_SCENARIOS:
                    saved = observations[provider].pop(scenario)
                    with self.assertRaisesRegex(RuntimeError, provider):
                        native.require_ssh_journeys(observations)
                    observations[provider][scenario] = saved
            self.assertEqual(set(native.SSH_SCENARIOS), {'sources', 'embedded-observe', 'embedded-control', 'fullscreen',
                                                       'disconnect', 'completion', 'cancellation', 'failure-return'})


if __name__ == '__main__':
    unittest.main()
