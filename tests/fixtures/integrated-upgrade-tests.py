#!/usr/bin/env python3
"""Check the host driver's admission and evidence rules without running hosts."""
import importlib.util
import base64
import copy
import fcntl
import gzip
import hashlib
import json
import lzma
import os
import shutil
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
import uuid
from unittest import mock

spec = importlib.util.spec_from_file_location('upgrade', Path(__file__).with_name('integrated-upgrade.py'))
upgrade = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upgrade)


class IntegratedUpgrade(unittest.TestCase):
    mode = 'prerequisites'

    def admit(self, item, work, attempt, candidate, source=None):
        return upgrade.require_paused(item, work, attempt, candidate, source, mode=self.mode)

    def evidence_retry(self, source, mode='prerequisites'):
        """Save the public shapes from a failed Plan check and its targeted Review."""
        candidate = 'a' * 40
        area = '.fluent/work/artifacts/work/attempt-1/'

        def save(name, value):
            path = source / (area + name)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(value if isinstance(value, str) else json.dumps(value))
            return dict(path=area + name, digest='sha256:' + upgrade.digest(path))

        row_id, command = ('plan:step-3', 'bash tests/host/check-integrated-upgrade-prerequisites.sh') if mode == 'prerequisites' else ('plan:step-4', 'bash tests/host/check-integrated-released-060-upgrade.sh')
        row = dict(id=row_id, responsibility='host', state='pending',
                   command={'command': command, 'working-directory': '.', 'source-row': row_id})
        matrix = save('write/writer-completion.json',
                      {'work-item-id': 'work', 'attempt-id': 'attempt-1', 'task-id': 'write', 'rows': [row]})
        evidence = save('host-evidence/failed.json', dict(schema_version=1, result='fail',
                        check=command, working_directory='.', output='native Pi prerequisite failed'))
        binding = save('plan-host-evidence/write/step-3/binding.json', {
            'schema-version': 1, 'work-item-id': 'work', 'attempt-id': 'attempt-1',
            'writer-task-id': 'write', 'candidate-commit': candidate, 'plan-row': row_id,
            'command': command, 'working-directory': '.', 'source-matrix': matrix,
            'evidence': evidence, 'result': 'fail'})
        review = save('review/review.md', 'Verdict: fail\nDisposition: evidence-needed\n'
                      'External-Condition: Restore the Pi host.\nAdditional-Proof: Retry the exact host command.\n')
        attachment = dict(snapshot_path=evidence['path'], digest=evidence['digest'])
        writer = dict(id='write', kind='write', status='complete', output={'commit': candidate},
                      artifact_area={'path': area + 'write'})
        item = dict(id='work', attempts=[dict(id='attempt-1', work_item_id='work', status='needs-user',
                    pause_kind='writer-completion', tasks=[writer])])
        initial = copy.deepcopy(item)
        attempt = item['attempts'][0]
        attempt.update(pause_kind='uncertain', review_state='failed', evidence_recoveries=[dict(
            id='recovery', candidate_commit=candidate, attachment=attachment, state='needs-evidence',
            targets=[dict(role='tests', review_task_id='review', review_digest=review['digest'],
                          prior_review_artifact=binding['path'])])])
        attempt['tasks'].append(dict(id='review', kind='review', role='tests', status='complete', output=None,
            artifact_area={'path': area + 'review'}, evidence_review_context=dict(
                recovery_id='recovery', candidate_commit=candidate, attachment=attachment,
                prior_review_artifact=binding['path'])))
        attempt['pause_record'] = {'candidate-commit': candidate, 'pause-kind': 'uncertain',
            'next-operation': {'kind': 'evidence-attach', 'arguments': [
                'attempt', 'evidence', 'attach', 'work', 'attempt-1', '--candidate', candidate,
                '--evidence-file', '<path>', '--review-artifact', review['path']]}}
        return initial, item

    def test_failed_host_review_admits_exact_public_evidence_retry(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            initial, retry = self.evidence_retry(source, self.mode)
            self.admit(initial, 'work', 'attempt-1', 'a' * 40, source)
            self.admit(retry, 'work', 'attempt-1', 'a' * 40, source)
            commands = mock.Mock(root=source, output=source / 'output', records=[])
            commands.output.mkdir()

            def run(name, argv, *args, **kwargs):
                commands.records.append(dict(name=name, status=0, group_settled=True))

            commands.run.side_effect = run
            args = SimpleNamespace(source_root=source, work='work', attempt='attempt-1', mode=self.mode)
            with mock.patch.object(upgrade, 'show', return_value=retry), \
                    mock.patch.object(upgrade, 'clean_candidate', return_value='a' * 40):
                upgrade.prerequisites(args, commands, 'a' * 40, 'fluent')
            self.assertEqual([r['name'] for r in commands.records], list(upgrade.CHECKS) + ['integrated-boundaries'])
            upgrade.require_complete(json.loads((commands.output / 'prerequisites.json').read_text()), 'a' * 40)

    def test_evidence_retry_rejects_unrelated_or_stale_public_state(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            _, item = self.evidence_retry(source, self.mode)
            for change in ('work', 'attempt', 'candidate', 'pause', 'operation', 'review',
                           'review-status', 'review-digest', 'recovery', 'evidence', 'context',
                           'later-task', 'later-recovery', 'foreign-task', 'pause-candidate'):
                with self.subTest(change=change):
                    bad = copy.deepcopy(item)
                    a = bad['attempts'][0]
                    r = a['evidence_recoveries'][0]
                    if change == 'work': bad['id'] = 'other'
                    if change == 'attempt': a['id'] = 'other'
                    if change == 'candidate': a['tasks'][0]['output']['commit'] = 'b' * 40
                    if change == 'pause': a['pause_kind'] = 'host-evidence'
                    if change == 'operation': a['pause_record']['next-operation']['arguments'][3] = 'other'
                    if change == 'review': r['targets'][0]['review_task_id'] = 'other'
                    if change == 'review-status': a['tasks'][-1]['status'] = 'executing'
                    if change == 'review-digest': r['targets'][0]['review_digest'] = 'sha256:stale'
                    if change == 'recovery': r['state'] = 'accepted'
                    if change == 'evidence': r['attachment']['digest'] = 'sha256:stale'
                    if change == 'context': a['tasks'][-1]['evidence_review_context']['candidate_commit'] = 'b' * 40
                    if change == 'later-task': a['tasks'].append(dict(kind='test', status='pending'))
                    if change == 'later-recovery': a['evidence_recoveries'].append(dict(id='later'))
                    if change == 'foreign-task': bad['attempts'].append(dict(id='attempt-2', tasks=[dict(status='executing')]))
                    if change == 'pause-candidate': a['pause_record']['candidate-commit'] = 'b' * 40
                    with self.assertRaises(RuntimeError):
                        self.admit(bad, 'work', 'attempt-1', 'a' * 40, source)

    def test_evidence_retry_rejects_stale_plan_bindings(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            _, retry = self.evidence_retry(source, self.mode)
            recovery = retry['attempts'][0]['evidence_recoveries'][0]
            path = source / recovery['targets'][0]['prior_review_artifact']
            original = path.read_bytes()
            binding = json.loads(original)
            for field, value in [('work-item-id', 'other'), ('attempt-id', 'other'),
                                 ('writer-task-id', 'other'), ('candidate-commit', 'b' * 40),
                                 ('plan-row', 'plan:step-4' if self.mode == 'prerequisites' else 'plan:step-3'), ('command', 'true'),
                                 ('working-directory', '/tmp'), ('result', 'pass')]:
                with self.subTest(field=field):
                    path.write_text(json.dumps(dict(binding, **{field: value})))
                    with self.assertRaisesRegex(RuntimeError, 'Plan evidence binding changed'):
                        self.admit(retry, 'work', 'attempt-1', 'a' * 40, source)
            path.write_bytes(original)

    def test_followup_evidence_retry_retains_the_original_plan_binding(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            _, item = self.evidence_retry(source, self.mode)
            attempt = item['attempts'][0]
            prior = attempt['tasks'][-1]
            prior_path = prior['artifact_area']['path'] + '/review.md'
            current = copy.deepcopy(prior)
            current['id'] = 'review-2'
            current['artifact_area']['path'] += '-2'
            review_path = current['artifact_area']['path'] + '/review.md'
            (source / review_path).parent.mkdir()
            (source / review_path).write_bytes((source / prior_path).read_bytes())
            recovery = copy.deepcopy(attempt['evidence_recoveries'][-1])
            recovery['id'] = 'recovery-2'
            recovery['targets'][0].update(review_task_id='review-2', prior_review_artifact=prior_path)
            current['evidence_review_context'].update(recovery_id='recovery-2', prior_review_artifact=prior_path)
            attempt['tasks'].append(current)
            attempt['evidence_recoveries'].append(recovery)
            attempt['pause_record']['next-operation']['arguments'][-1] = review_path
            self.admit(item, 'work', 'attempt-1', 'a' * 40, source)
            (source / prior_path).write_text('Verdict: pass\n')
            with self.assertRaisesRegex(RuntimeError, 'digest changed'):
                self.admit(item, 'work', 'attempt-1', 'a' * 40, source)

    def test_evidence_retry_rejects_changed_bound_files(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            _, item = self.evidence_retry(source, self.mode)
            files = list(source.rglob('*.*'))
            for path in files:
                if not path.is_file():
                    continue
                with self.subTest(path=path.name):
                    original = path.read_bytes()
                    path.write_text('{}')
                    try:
                        with self.assertRaises((RuntimeError, KeyError)):
                            self.admit(item, 'work', 'attempt-1', 'a' * 40, source)
                    finally:
                        path.write_bytes(original)

    def test_evidence_retry_rejects_live_owners_even_after_tasks_complete(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            initial, retry = self.evidence_retry(source, self.mode)
            area = source / '.fluent/work/artifacts/work/attempt-1/write'
            execution = area / 'execution.json'
            execution.write_text(json.dumps(dict(state='settled', settled_at='finished',
                attempt_owner=dict(project_root=str(source.resolve()), work_item_id='work', attempt_id='attempt-1'))))
            locks = [area / 'execution-owner.lock', source / '.fluent/work/locks/work/write.lock',
                     source / '.fluent/work/locks/work/attempt-1-learner.lock']
            for item in (initial, retry):
                for path in locks:
                    with self.subTest(pause=item['attempts'][0]['pause_kind'], lock=path.name):
                        path.parent.mkdir(parents=True, exist_ok=True)
                        with path.open('w') as lock:
                            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            with self.assertRaisesRegex(RuntimeError, 'ownership'):
                                self.admit(item, 'work', 'attempt-1', 'a' * 40, source)
                        self.admit(item, 'work', 'attempt-1', 'a' * 40, source)
            execution.write_text(json.dumps(dict(state='running')))
            with self.assertRaisesRegex(RuntimeError, 'durable execution settlement'):
                self.admit(retry, 'work', 'attempt-1', 'a' * 40, source)

    def test_driver_rejects_candidate_or_public_state_drift_before_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            _, retry = self.evidence_retry(source, self.mode)
            args = SimpleNamespace(source_root=source, work='work', attempt='attempt-1', mode=self.mode)
            commands = mock.Mock(root=source, output=source / 'output', records=[])
            commands.output.mkdir()
            for outputs, reason in [(['b' * 40], 'candidate changed'),
                                    (['a' * 40, ' M file'], 'candidate must be clean')]:
                with mock.patch.object(upgrade, 'read_command', side_effect=outputs):
                    with self.assertRaisesRegex(RuntimeError, reason):
                        upgrade.prerequisites(args, commands, 'a' * 40, 'fluent')
                commands.run.assert_not_called()
            changed = copy.deepcopy(retry)
            changed['attempts'][0]['tasks'][-1]['status'] = 'executing'
            with mock.patch.object(upgrade, 'show', side_effect=[retry, changed, changed, changed]), \
                    mock.patch.object(upgrade, 'clean_candidate', return_value='a' * 40):
                with self.assertRaisesRegex(RuntimeError, 'executing'):
                    upgrade.prerequisites(args, commands, 'a' * 40, 'fluent')
            commands.run.assert_not_called()
            self.assertFalse(json.loads((commands.output / 'prerequisites.json').read_text())['settled'])

    def test_retry_rejects_the_other_host_mode(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            other = 'released' if self.mode == 'prerequisites' else 'prerequisites'
            _, retry = self.evidence_retry(source, other)
            with self.assertRaisesRegex(RuntimeError, 'failed host command'):
                self.admit(retry, 'work', 'attempt-1', 'a' * 40, source)

    def test_requires_every_constituent(self):
        expected = {'claude-writer-session-upgrade', 'claude-real-legacy-upgrade',
                    'node-worker-upgrade', 'worker-input-upgrade', 'completion-repair-admission',
                    'semantic-completion-recovery', 'native-acceptance-prerequisites',
                    'native-codex-display-backpressure', 'ssh-fullscreen-backpressure', 'dashboard-terminal'}
        self.assertEqual(set(upgrade.CHECKS), expected)
        receipt = {'candidate': 'a' * 40, 'checks': [dict(name=n, status=0) for n in upgrade.CHECKS] +
                   [dict(name='integrated-boundaries', status=0)], 'settled': True}
        upgrade.require_complete(receipt, 'a' * 40)
        for changed in ('missing', 'failure', 'candidate', 'cleanup'):
            bad = copy.deepcopy(receipt)
            if changed == 'missing': bad['checks'].pop()
            if changed == 'failure': bad['checks'][0]['status'] = 1
            if changed == 'candidate': bad['candidate'] = 'b' * 40
            if changed == 'cleanup': bad['settled'] = False
            with self.assertRaises(RuntimeError): upgrade.require_complete(bad, 'a' * 40)

    def test_requires_paused_exact_attempt_with_no_executing_tasks(self):
        item = {'id': 'work', 'attempts': [{'id': 'attempt-1', 'status': 'needs-user',
                'pause_kind': 'writer-completion', 'tasks': [{'status': 'complete',
                'output': {'commit': 'a' * 40}}]}]}
        self.admit(item, 'work', 'attempt-1', 'a' * 40)
        for field, value in [('status', 'executing'), ('pause_kind', 'cancelled'), ('id', 'attempt-2')]:
            bad = copy.deepcopy(item)
            bad['attempts'][0][field] = value
            with self.assertRaises(RuntimeError): self.admit(bad, 'work', 'attempt-1', 'a' * 40)
        item['attempts'].append({'id': 'attempt-2', 'tasks': [{'status': 'executing'}]})
        with self.assertRaises(RuntimeError): self.admit(item, 'work', 'attempt-1', 'a' * 40)

    def test_snapshot_checks_bytes_and_refuses_escape_and_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            entries = {}
            for i in range(26):
                path = root / str(i)
                path.write_text(str(i))
                entries[str(i)] = hashlib.sha256(path.read_bytes()).hexdigest()
            self.assertEqual(upgrade.verify_snapshot(root, entries), entries)
            for name in ('../outside', '/absolute', 'claude-home/secret'):
                bad = dict(entries)
                bad[name] = bad.pop('0')
                with self.assertRaises(RuntimeError): upgrade.verify_snapshot(root, bad)
            (root / '0').unlink()
            (root / '0').symlink_to(root / '1')
            with self.assertRaises(RuntimeError): upgrade.verify_snapshot(root, entries)
            (root / '0').unlink()
            (root / '0').write_text('changed')
            with self.assertRaises(RuntimeError): upgrade.verify_snapshot(root, entries)

    def test_driver_requires_explicit_host_opt_in_before_any_work(self):
        import os
        environment = {k:v for k,v in os.environ.items() if k != 'FLUENT_INTEGRATED_UPGRADE_HOST'}
        for script in ('check-integrated-upgrade-prerequisites.sh', 'check-integrated-released-060-upgrade.sh'):
            result = subprocess.run(['bash', str(Path(__file__).parents[1] / 'host' / script)],
                                    env=environment, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('FLUENT_INTEGRATED_UPGRADE_HOST', result.stderr)

    def test_evidence_index_does_not_scan_private_provider_homes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'command.log').write_text('complete command output')
            (root / 'claude-home').mkdir()
            (root / 'claude-home' / 'secret').write_text('never collect')
            self.assertEqual(set(upgrade.direct_evidence(root)), {'command.log'})

    def test_command_failure_and_timeout_retain_complete_output(self):
        import os
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = upgrade.Commands(root, root, dict(os.environ))
            with self.assertRaisesRegex(RuntimeError, 'failed'):
                commands.run('failed', ['/bin/sh', '-c', 'echo retained; exit 7'])
            self.assertEqual(commands.records[0]['status'], 7)
            self.assertEqual((root / '00-failed.log').read_text(), 'retained\n')
            with self.assertRaises(subprocess.TimeoutExpired):
                commands.run('timeout', ['/bin/sleep', '60'], timeout=0.05)
            record = json.loads((root / '01-timeout.json').read_text())
            self.assertTrue(record['group_settled'])
            self.assertTrue(record['timed_out'])
            self.assertIsNone(record['status'])

    def test_machine_reads_quiet_guidance_without_changing_execution_environment(self):
        import os
        import sys
        for inherited in (None, '0'):
            with self.subTest(inherited=inherited), tempfile.TemporaryDirectory() as directory, \
                    mock.patch.dict(os.environ):
                if inherited is None:
                    os.environ.pop('FLUENT_QUIET', None)
                else:
                    os.environ['FLUENT_QUIET'] = inherited
                root = Path(directory)
                initial, _ = self.evidence_retry(root, self.mode)
                (root / 'status.json').write_text(json.dumps(initial))
                binary = root / 'fluent'
                binary.write_text(f'#!{sys.executable}\n'
                    'import json, os, sys\nfrom pathlib import Path\n'
                    "assert sys.argv[1:] == ['work-item', 'show', 'work']\n"
                    "Path('environment.json').write_text(json.dumps({key: os.environ.get(key) "
                    "for key in ('FLUENT_QUIET', 'FLUENT_NO_UPDATE_CHECK')}))\n"
                    "print(Path('status.json').read_text())\n")
                binary.chmod(0o755)
                # Observe a real read subprocess and retain its deadline and JSON guards.
                with mock.patch.object(upgrade.subprocess, 'check_output',
                                       wraps=subprocess.check_output) as read:
                    item = upgrade.show(binary, root, 'work')
                self.assertEqual(read.call_args.kwargs['timeout'], 30)
                self.assertEqual(json.loads((root / 'environment.json').read_text()),
                                 dict(FLUENT_QUIET='1', FLUENT_NO_UPDATE_CHECK='1'))
                self.assertEqual(item, initial)
                self.admit(item, 'work', 'attempt-1', 'a' * 40, root)
                with self.assertRaisesRegex(RuntimeError, 'exact candidate'):
                    self.admit(item, 'work', 'attempt-1', 'b' * 40, root)
                (root / 'status.json').write_text('invalid JSON')
                with self.assertRaises(json.JSONDecodeError):
                    upgrade.show(binary, root, 'work')

                self.assertEqual(os.environ.get('FLUENT_QUIET'), inherited)
                commands = upgrade.Commands(root, root, dict(os.environ))
                log = commands.run('execution-environment', [sys.executable, '-c',
                    "import json, os; print(json.dumps(os.environ.get('FLUENT_QUIET')))"])
                self.assertEqual(json.loads(log.read_text()), inherited)

    def test_public_read_retains_streams_git_trace_and_identity(self):
        import sys
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            diagnostics = root / 'reads'
            binary = root / 'fluent'
            binary.write_text(f'#!{sys.executable}\n'
                'import json, os, subprocess, sys\n'
                "assert sys.argv[1:] == ['work-item', 'show', 'work']\n"
                "assert os.environ['FLUENT_QUIET'] == '1'\n"
                "assert os.environ['FLUENT_NO_UPDATE_CHECK'] == '1'\n"
                "subprocess.check_call(['git', '--version'], stdout=sys.stderr)\n"
                "print(json.dumps({'id': 'work'}))\n")
            binary.chmod(0o755)
            with mock.patch.object(upgrade, 'READ_DIAGNOSTICS', diagnostics, create=True):
                self.assertEqual(upgrade.show(binary, root, 'work'), {'id': 'work'})
            records = list(diagnostics.glob('*/read.json'))
            self.assertEqual(len(records), 1)
            record = json.loads(records[0].read_text())
            self.assertEqual(record['argv'], [str(binary), 'work-item', 'show', 'work'])
            self.assertEqual(record['cwd'], str(root))
            self.assertEqual(record['deadline_seconds'], 30)
            self.assertEqual(record['status'], 0)
            self.assertFalse(record['timed_out'])
            self.assertTrue(record['group_settled'])
            self.assertTrue(record['output_complete'])
            self.assertGreater(record['elapsed_seconds'], 0)
            for name, sha in record['sha256'].items():
                self.assertEqual(upgrade.digest(records[0].parent / name), sha)
            self.assertEqual(json.loads((records[0].parent / 'stdout.log').read_text()),
                             {'id': 'work'})
            self.assertIn('git version', (records[0].parent / 'stderr.log').read_text())
            trace = [json.loads(line) for line in
                     (records[0].parent / 'git-trace2.jsonl').read_text().splitlines()]
            self.assertTrue(any(event['event'] == 'start' for event in trace))
            self.assertTrue(any(event['event'] == 'exit' for event in trace))

    def test_public_read_failure_retains_complete_streams(self):
        import sys
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            diagnostics = root / 'reads'
            with mock.patch.object(upgrade, 'READ_DIAGNOSTICS', diagnostics, create=True):
                with self.assertRaises(subprocess.CalledProcessError) as failure:
                    upgrade.read_command([sys.executable, '-c',
                        "import sys; print('out' * 10000); "
                        "print('err' * 10000, file=sys.stderr); sys.exit(7)"], root)
            self.assertEqual(failure.exception.returncode, 7)
            record_path, = diagnostics.glob('*/read.json')
            record = json.loads(record_path.read_text())
            self.assertEqual(record['status'], 7)
            self.assertTrue(record['output_complete'])
            self.assertEqual((record_path.parent / 'stdout.log').read_text(), 'out' * 10000 + '\n')
            self.assertEqual((record_path.parent / 'stderr.log').read_text(), 'err' * 10000 + '\n')

    def test_public_read_timeout_retains_failure_and_settles_child(self):
        import sys
        import time
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            diagnostics = root / 'reads'
            wait = subprocess.Popen.wait
            deadlines = []

            def bounded_wait(child, timeout=None):
                deadlines.append(timeout)
                if timeout == 30:
                    deadline = time.monotonic() + 5
                    while not (root / 'ready').exists():
                        self.assertLess(time.monotonic(), deadline, 'read fixture did not start')
                        time.sleep(0.01)
                return wait(child, timeout=0.05 if timeout == 30 else timeout)

            with mock.patch.object(upgrade, 'READ_DIAGNOSTICS', diagnostics, create=True), \
                    mock.patch.object(subprocess.Popen, 'wait', bounded_wait):
                with self.assertRaises(subprocess.TimeoutExpired):
                    upgrade.read_command([sys.executable, '-c',
                        "import sys, time; from pathlib import Path; "
                        "print('before wait', flush=True); "
                        "print('diagnostic', file=sys.stderr, flush=True); "
                        "Path('ready').touch(); time.sleep(60)"], root)
            self.assertEqual(deadlines[0], 30)
            record_path, = diagnostics.glob('*/read.json')
            record = json.loads(record_path.read_text())
            self.assertTrue(record['timed_out'])
            self.assertNotEqual(record['status'], 0)
            self.assertTrue(record['group_settled'])
            self.assertTrue(record['output_complete'])
            self.assertIn('before wait', (record_path.parent / 'stdout.log').read_text())
            self.assertIn('diagnostic', (record_path.parent / 'stderr.log').read_text())
            with self.assertRaises(ProcessLookupError):
                os.kill(record['pid'], 0)

    def test_public_read_rejects_unsettled_group_without_complete_evidence(self):
        import sys
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            diagnostics = root / 'reads'
            with mock.patch.object(upgrade, 'READ_DIAGNOSTICS', diagnostics), \
                    mock.patch.object(upgrade, 'settle_coordinator', return_value=False):
                with self.assertRaisesRegex(RuntimeError, 'live group'):
                    upgrade.read_command([sys.executable, '-c', "print('output')"], root)
            record_path, = diagnostics.glob('*/read.json')
            record = json.loads(record_path.read_text())
            self.assertEqual(record['status'], 0)
            self.assertFalse(record['output_complete'])
            self.assertFalse(record['group_settled'])
            self.assertEqual(record['sha256'], {})

    def test_reaped_coordinator_never_authorizes_group_signals(self):
        import os
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = upgrade.Commands(root, root, dict(os.environ))
            # Model a numeric group still present after wait reaped the child.
            # It may now belong to somebody else. A probe grants no authority.
            with mock.patch.object(upgrade.os, 'killpg') as kill:
                with self.assertRaisesRegex(RuntimeError, 'failed'):
                    commands.run('reaped', ['/bin/sh', '-c', 'exit 7'])
            self.assertTrue(all(call.args[1] == 0 for call in kill.call_args_list),
                            'a reaped coordinator cannot authorize signaling a numeric group')

    def test_failed_cancellation_retains_receipt_without_forced_coordinator_cleanup(self):
        import os
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = upgrade.Commands(root, root, dict(os.environ))
            child = mock.Mock(returncode=None)
            child.pid = 12345
            child.wait.side_effect = subprocess.TimeoutExpired('foreground', 1)
            with mock.patch.object(upgrade, 'OwnedAttempt') as owner, \
                    mock.patch.object(upgrade.subprocess, 'Popen', return_value=child), \
                    mock.patch.object(upgrade.os, 'killpg') as kill:
                owner.return_value.cancel.side_effect = RuntimeError('setup still owns its lease')
                with self.assertRaisesRegex(RuntimeError, 'setup still owns'):
                    commands.run('foreground', ['fluent', 'attempt', 'run', 'work', 'attempt-1'])
                kill.assert_not_called()
            receipt = json.loads((root / '00-foreground.json').read_text())
            self.assertFalse(receipt['group_settled'])
            self.assertEqual(receipt['cleanup_error'], 'setup still owns its lease')
            self.assertIn('log_sha256', receipt)

    def test_host_timeout_requires_its_owner_to_settle(self):
        import os
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = upgrade.Commands(root, root, dict(os.environ))
            child = mock.Mock(returncode=None, pid=12345)
            child.wait.side_effect = subprocess.TimeoutExpired('host', 1)
            with mock.patch.object(upgrade.subprocess, 'Popen', return_value=child), \
                    mock.patch.object(upgrade.os, 'killpg') as kill:
                with self.assertRaisesRegex(RuntimeError, 'its owner must settle'):
                    commands.run('host', ['bash', 'host.sh'], host_check=True)
                kill.assert_not_called()
            receipt = json.loads((root / '00-host.json').read_text())
            self.assertTrue(receipt['cleanup_pending'])
            self.assertFalse(receipt['group_settled'])
            self.assertFalse(receipt['log_complete'])
            self.assertEqual(upgrade.digest(receipt['log_snapshot']), receipt['log_sha256'])

    def test_cancel_requires_released_generation_and_setup_owners(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            owner = upgrade.OwnedAttempt(['fluent', 'attempt', 'run', 'work', 'attempt-1'], root, {})
            before = dict(attempt={'tasks': [{'status': 'executing'}]}, setup={'writer': True},
                          executions={'execution.json': {'held': True, 'record':
                                      {'launch_generation': 'original', 'state': 'running'}}})
            after = dict(attempt={'tasks': [{'status': 'needs-user'}], 'cancellation':
                                 {'requested_at': 'requested', 'settled_at': 'settled'}},
                         setup={'writer': False}, executions={'execution.json':
                         {'held': False, 'record': {'launch_generation': 'original',
                                                   'state': 'canceled', 'settled_at': 'settled'}}})
            for change in ('generation', 'execution', 'setup', 'task', 'stop'):
                bad = copy.deepcopy(after)
                if change == 'generation': bad['executions']['execution.json']['record']['launch_generation'] = 'other'
                if change == 'execution': bad['executions']['execution.json']['held'] = True
                if change == 'setup': bad['setup']['writer'] = True
                if change == 'task': bad['attempt']['tasks'][0]['status'] = 'executing'
                if change == 'stop': bad['attempt']['cancellation'].pop('settled_at')
                receipt = {}
                with mock.patch.object(owner, 'snapshot', side_effect=[before, bad]), \
                        mock.patch.object(upgrade.subprocess, 'run', return_value=mock.Mock(returncode=0)):
                    with self.assertRaises(RuntimeError):
                        owner.cancel(root / change, receipt)
                self.assertFalse(receipt['settled'])


class ReleasedFixture(unittest.TestCase):
    """Exercise the real released driver with a disposable public-coordinator double."""
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.fixture = self.root / 'real-060-node-claude-upgrade'
        self.source = self.fixture / 'main'
        self.source.mkdir(parents=True)
        self.git('init', '-q')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'user.name', 'Fixture')
        self.save(self.source / '.gitignore', '.fluent/work/\n')
        self.save(self.source / 'package.json', '{"scripts":{"test":"node --test"}}')
        self.save(self.source / 'package-lock.json', '{"lockfileVersion":3}')
        self.save(self.source / 'vendor/example/index.js', 'exports.value = 1;\n')
        self.save(self.source / 'AGENTS.md', 'Preserve the regression.\n')
        self.save(self.source / '.fluent/config.yaml', 'coder: claude\n')
        self.save(self.source / '.fluent/tester.yaml', 'commands:\n  - command: npm ci --offline --ignore-scripts --no-audit --no-fund && npm test\n    test_harness: shell-harness\n')
        self.save(self.source / 'story.js', 'exports.story = "before";\n')
        self.save(self.source / 'README.md', 'Example story\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'Retain initial package')
        self.seed = self.git('rev-parse', 'HEAD')
        self.save(self.source / 'story.test.js', 'require("node:assert").equal(require("./story").story, "after");\n')
        self.git('add', 'story.test.js')
        self.git('commit', '-qm', 'Add story regression')
        self.initial = self.git('rev-parse', 'HEAD')
        (self.source / 'story.js').write_text('exports.story = "after";\n')
        self.commit_output = self.git('commit', '-am', 'Continue package')
        self.tip = self.git('rev-parse', 'HEAD')
        self.workspace = self.fixture / 'candidate'
        self.git('worktree', 'add', '--detach', str(self.workspace), self.tip)
        self.binary = self.root / 'integration-binary'
        self.binary.write_text('controlled integration binary')
        released = self.root / 'released-binary'
        released.write_text('controlled released binary')
        self.candidate = 'a' * 40
        self.work, self.attempt_id = 'story-upgrade', 'attempt-2'
        self.area = '.fluent/work/artifacts/story-upgrade/attempt-2'
        def task(name, kind, commit):
            return dict(id=name, kind=kind, role='tester' if kind == 'tester' else 'writer', status='complete', output={'commit': commit},
                        work_item_id=self.work, attempt_id=self.attempt_id,
                        artifact_area={'path': self.area + '/' + name},
                        workspace_access={'writes': [{'path': str(self.workspace)}]})
        self.old_task = task('write-1', 'write', self.initial)
        self.old_task['output']['base_commit'] = self.seed
        self.writer = task('write-2', 'write', self.tip)
        self.writer['output']['base_commit'] = self.initial
        self.tester = task('tester-2', 'tester', self.tip)
        self.tester['review_context'] = dict(candidate_commit=self.tip)
        self.tester['output'] = None
        self.before = dict(id=self.work, attempts=[dict(id='attempt-1', status='failed', tasks=[]),
            dict(id=self.attempt_id, work_item_id=self.work, status='needs-user', tasks=[self.old_task],
                 artifacts=[dict(producer_id=self.old_task['id'], path=self.initial)])], merge_candidates=[])
        self.after = copy.deepcopy(self.before)
        self.after['attempts'][-1].update(status='complete', review_state='passed', learning={'status':'succeeded'},
                                        tasks=[self.old_task, self.writer, self.tester])
        self.after['merge_candidates'] = [dict(attempt_id=self.attempt_id, ready_at='now',
                                              merge_state={'status':'pending'}, candidate_commit=self.tip)]
        self.item_path = self.source / '.fluent/work/items/story-upgrade.json'
        self.save(self.item_path, self.before)
        self.foreign = self.source / '.fluent/work/items/other.json'
        self.save(self.foreign, {'id':'other'})
        self.old_artifact = self.source / (self.area + '/write-1/transcript.jsonl')
        self.save(self.old_artifact, 'retained initial execution\n')
        self.old_log = self.old_artifact.parent / 'commands/0-stdout.log'
        self.save(self.old_log, 'retained command output\n')
        config = self.source / '.fluent/config.yaml'
        self.save(config, 'coder: claude\n')
        self.save(self.source / '.fluent/tester.yaml', 'commands:\n  - command: npm ci --offline --ignore-scripts --no-audit --no-fund && npm test\n    test_harness: shell-harness\n')
        failure = self.source / (self.area + '/write-1/failure.json')
        self.save(failure, dict(session_id=upgrade.MISSING_SESSION, num_turns=0, errors=['No conversation found']))
        marker = self.fixture / 'stage-2'
        marker.write_text('advanced by fixture owner')
        snapshot = self.root / 'snapshot'
        snapshot.mkdir()
        for index in range(26): (snapshot / str(index)).write_text(str(index))
        prerequisite = self.root / 'prerequisites.json'
        self.save(prerequisite, dict(candidate=self.candidate, settled=True,
            checks=[dict(name=name, status=0) for name in (*upgrade.CHECKS, 'integrated-boundaries')]))
        self.manifest = dict(fixture=str(self.fixture), project='main', work=self.work, attempt=self.attempt_id,
            initial_commit=self.initial, missing_session=upgrade.MISSING_SESSION, released_sha256=upgrade.digest(released),
            released_binary=str(released), snapshot_root=str(snapshot),
            snapshot_files={p.name:upgrade.digest(p) for p in snapshot.iterdir()},
            prerequisites=str(prerequisite), prerequisites_sha256=upgrade.digest(prerequisite),
            config=str(config.relative_to(self.fixture)), config_sha256=upgrade.digest(config),
            original_failure=str(failure.relative_to(self.fixture)), original_failure_sha256=upgrade.digest(failure),
            stage2_marker='stage-2', stage2_marker_sha256=upgrade.digest(marker))
        self.results_path = self.source / self.tester['artifact_area']['path'] / 'tester-results.json'
        self.results = dict(candidate_commit=self.tip, error=None,
            commands=[dict(command=command, test_harness='shell-harness', exit_code=0,
                           stdout_log=f'command-{index}.stdout.log', stderr_log=f'command-{index}.stderr.log')
                      for index, command in enumerate(('npm ci --offline --ignore-scripts --no-audit --no-fund', 'npm test'))],
            tests=[dict(id='package', status='pass')], summary=dict(total=1, **{'pass':1}, fail=0, skipped=0))
        self.after['attempts'][-1]['artifacts'].extend([
            dict(producer_id=self.writer['id'], path=self.tip),
            dict(producer_id=self.tester['id'], path=str(self.results_path.relative_to(self.source)))])
        self.writer_log = self.source / self.writer['artifact_area']['path'] / 'transcript.jsonl'
        self.selection_path = self.root / 'package-selection.json'
        npm_target = self.root / 'npm-cli.js'
        npm_target.write_text('disposable installed npm runtime')
        npm = self.root / 'npm'
        npm.symlink_to(npm_target)
        patch = mock.patch.object(upgrade.shutil, 'which', return_value=str(npm))
        patch.start(); self.addCleanup(patch.stop)
        self.manifest_path = self.root / 'manifest.json'
        self.save(self.manifest_path, self.manifest)
        self.args = SimpleNamespace(fixture=self.fixture, fixture_manifest=self.manifest_path, verify_only=False,
                                    continuation_output=None, package_proofs=None)
        self.output = self.root / 'output'
        self.output.mkdir()
        self.commands = SimpleNamespace(root=self.source, output=self.output, run=self.run_command)
        self.calls = []
        self.on_resume = self.materialize_results
        for name, value in [('INITIAL_COMMIT', self.initial), ('RELEASE_SHA256', upgrade.digest(released))]:
            patch = mock.patch.object(upgrade, name, value)
            patch.start(); self.addCleanup(patch.stop)
        patch = mock.patch.object(upgrade, 'show', side_effect=lambda *a: json.loads(self.item_path.read_text()))
        patch.start(); self.addCleanup(patch.stop)
        patch = mock.patch.object(upgrade, 'clean_candidate', return_value=self.candidate)
        patch.start(); self.addCleanup(patch.stop)

    def save(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value if isinstance(value,str) else json.dumps(value))

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.source, stderr=subprocess.PIPE, text=True).strip()

    def run_command(self, name, argv, cwd=None, **kwargs):
        self.calls.append(name)
        if name == 'public-resume':
            self.save(self.item_path, self.after)
            self.on_resume()
        else:
            subprocess.run([str(a) for a in argv], cwd=cwd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def materialize_results(self):
        self.save(self.results_path, self.results)
        for entry in self.results['commands']:
            for stream in ('stdout_log', 'stderr_log'):
                self.save(self.results_path.parent / entry[stream], 'original npm package output\n')
        self.native_events = [dict(type='system', subtype='init', session_id='native-session',
            cwd=str((self.source / self.writer['workspace_access']['writes'][0]['path']).resolve()))]
        for index, command in enumerate(upgrade.PACKAGE_COMMANDS):
            self.native_events.extend([
                dict(type='assistant', session_id='native-session', parent_tool_use_id=None,
                     message={'content': [dict(type='tool_use', id=f'tool-{index}', name='Bash',
                                               input={'command': command})]}),
                dict(type='user', session_id='native-session', parent_tool_use_id=None,
                     message={'content': [dict(type='tool_result', tool_use_id=f'tool-{index}',
                                               is_error=False, content='native npm output')]},
                     tool_use_result=dict(stdout='native npm output', stderr='', interrupted=False,
                                          isImage=False, noOutputExpected=False))])
        self.native_events.extend([
            dict(type='assistant', session_id='native-session', parent_tool_use_id=None,
                 message={'content': [dict(type='tool_use', id='producing-commit', name='Bash',
                     input={'command': 'git add story.js README.md && git commit -m "Continue package"'})]}),
            dict(type='user', session_id='native-session', parent_tool_use_id=None,
                 message={'content': [dict(type='tool_result', tool_use_id='producing-commit',
                                          is_error=False, content=self.commit_output)]},
                 tool_use_result=dict(stdout=self.commit_output, stderr='', interrupted=False,
                                      isImage=False, noOutputExpected=False))])
        self.native_events.append(dict(type='result', subtype='success', is_error=False,
                                       session_id='native-session'))
        self.save_native_events()
        self.execution_path = self.writer_log.parent / 'execution.json'
        self.save(self.execution_path, dict(schema_version=2, state='settled', settled_at='finished',
            launch_generation='b' * 32, leader_pid=123, process_group_id=123,
            attempt_owner=dict(project_root=str(self.source), work_item_id=self.work, attempt_id=self.attempt_id)))
        self.save(self.writer_log.parent / 'execution-receipt', '2 ' + 'b' * 32 + ' 0000000123\n')

    def save_native_events(self):
        self.save(self.writer_log, '\n'.join(json.dumps(row) for row in self.native_events) + '\n')

    def add_native_ui_preamble(self):
        self.native_events.insert(0, dict(type='system', subtype='ui_invalidate',
            event='ui.render', session_id='native-session'))
        for row in self.native_events:
            row['uuid'] = str(uuid.uuid4())

    def test_native_preamble_accepts_same_session_notification(self):
        self.continue_fixture()
        self.add_native_ui_preamble()
        self.assertEqual(len({row['uuid'] for row in self.native_events}), len(self.native_events))
        self.save_native_events()
        self.select_proofs()
        self.exercise()
        result = json.loads((self.output / 'writer-package-results.json').read_text())
        self.assertEqual(result['session_id'], 'native-session')
        self.assertEqual(result['producing_commit'], self.tip)

    def test_native_preamble_rejects_unknown_or_unbound_events(self):
        self.continue_fixture()
        self.add_native_ui_preamble()
        events = copy.deepcopy(self.native_events)
        for change in ('foreign-session', 'missing-session', 'unknown-subtype', 'unknown-event',
                       'extra-payload', 'tool-before-init', 'result-before-init', 'duplicate-init',
                       'missing-init', 'foreign-init-session', 'wrong-cwd', 'relative-cwd',
                       'missing-uuid', 'invalid-uuid'):
            with self.subTest(change=change):
                self.native_events = copy.deepcopy(events)
                preamble, init = self.native_events[:2]
                if change == 'foreign-session': preamble['session_id'] = 'foreign'
                if change == 'missing-session': del preamble['session_id']
                if change == 'unknown-subtype': preamble['subtype'] = 'unknown'
                if change == 'unknown-event': preamble['event'] = 'tool.execute'
                if change == 'extra-payload': preamble['payload'] = {'command': 'npm test'}
                if change == 'tool-before-init': self.native_events.insert(1, self.native_events.pop(2))
                if change == 'result-before-init': self.native_events.insert(1, self.native_events.pop(3))
                if change == 'duplicate-init': self.native_events.insert(2, copy.deepcopy(init))
                if change == 'missing-init': del self.native_events[1]
                if change == 'foreign-init-session': init['session_id'] = 'foreign'
                if change == 'wrong-cwd': init['cwd'] = str(self.root)
                if change == 'relative-cwd': init['cwd'] = '.'
                if change == 'missing-uuid': del preamble['uuid']
                if change == 'invalid-uuid': preamble['uuid'] = 'not-an-event-uuid'
                self.save_native_events()
                self.select_proofs()
                with self.assertRaises(RuntimeError):
                    self.exercise()

    def add_native_commit_metadata(self):
        self.native_events[6]['tool_use_result']['gitOperation'] = dict(commit=dict(
            sha=self.tip[:7], kind='committed', branch=self.git('branch', '--show-current')))

    def test_native_commit_metadata_binds_success_to_producing_object(self):
        self.continue_fixture()
        self.add_native_commit_metadata()
        for sha in (self.tip[:7], self.tip):
            with self.subTest(sha=sha):
                native = self.native_events[6]['tool_use_result']
                native['gitOperation']['commit']['sha'] = sha
                self.assertNotIn('exitCode', native)
                self.save_native_events()
                self.select_proofs()
                self.exercise()
                result = json.loads((self.output / 'writer-package-results.json').read_text())
                self.assertEqual(result['producing_commit'], self.tip)
                self.assertEqual(result['commit_tool_use_id'], 'producing-commit')

    def test_native_commit_metadata_rejects_mismatches_and_npm_metadata(self):
        self.continue_fixture()
        self.add_native_commit_metadata()
        events = copy.deepcopy(self.native_events)
        for change in ('unknown-operation', 'unknown-field', 'null-operation', 'null-commit',
                       'missing-sha', 'missing-kind', 'missing-branch', 'wrong-kind', 'wrong-sha',
                       'invalid-sha', 'unknown-sha', 'wrong-branch', 'failed', 'missing-status',
                       'interrupted', 'nonzero', 'foreign-result', 'stale-commit', 'after-commit',
                       'npm-ci-metadata', 'npm-test-metadata'):
            with self.subTest(change=change):
                self.native_events = copy.deepcopy(events)
                result = self.native_events[6]
                native = result['tool_use_result']
                metadata = native['gitOperation']['commit']
                if change == 'unknown-operation': native['gitOperation']['checkout'] = {}
                if change == 'unknown-field': metadata['unknown'] = 'payload'
                if change == 'null-operation': native['gitOperation'] = None
                if change == 'null-commit': native['gitOperation']['commit'] = None
                if change.startswith('missing-') and change != 'missing-status':
                    del metadata[change.removeprefix('missing-')]
                if change == 'wrong-kind': metadata['kind'] = 'unchanged'
                if change == 'wrong-sha': metadata['sha'] = self.initial[:7]
                if change == 'invalid-sha': metadata['sha'] = 'HEAD'
                if change == 'unknown-sha': metadata['sha'] = '0' * 40
                if change == 'wrong-branch': metadata['branch'] = 'foreign-branch'
                if change == 'failed': result['message']['content'][0]['is_error'] = True
                if change == 'missing-status': del result['message']['content'][0]['is_error']
                if change == 'interrupted': native['interrupted'] = True
                if change == 'nonzero': native['exitCode'] = 1
                if change == 'foreign-result': result['message']['content'][0]['tool_use_id'] = 'foreign'
                if change == 'stale-commit':
                    metadata['sha'] = self.initial[:7]
                    native['stdout'] = native['stdout'].replace(self.tip[:7], self.initial[:7])
                if change == 'after-commit': self.native_events[1:7] = [*events[5:7], *events[1:5]]
                if change in ('npm-ci-metadata', 'npm-test-metadata'):
                    position = 2 if change == 'npm-ci-metadata' else 4
                    self.native_events[position]['tool_use_result']['gitOperation'] = copy.deepcopy(native['gitOperation'])
                self.save_native_events()
                self.select_proofs()
                with self.assertRaises((RuntimeError, subprocess.CalledProcessError)):
                    self.exercise()

    def use_empty_shell_results(self):
        # Preserve the canonical shell extractor's empty summary for Node's
        # checkmark/info output; do not invent parsed test successes.
        self.results['tests'] = []
        self.results['summary'] = dict(total=0, **{'pass': 0}, fail=0, skipped=0)
        self.save(self.results_path, self.results)
        outputs = ('\nadded 1 package in 120ms\n',
            '\n> writer-package-upgrade-fixture@1.0.0 test\n> node --test story.test.js\n\n'
            '✔ installed local dependency (0.310792ms)\n'
            '✔ simple title (0.097375ms)\n'
            '✔ trims and joins whitespace runs (0.087ms)\n'
            'ℹ tests 3\nℹ suites 0\nℹ pass 3\nℹ fail 0\nℹ cancelled 0\n'
            'ℹ skipped 0\nℹ todo 0\nℹ duration_ms 43.737958\n')
        for command, stdout in zip(self.results['commands'], outputs):
            self.save(self.results_path.parent / command['stdout_log'], stdout)
            self.save(self.results_path.parent / command['stderr_log'], '')

    def test_empty_shell_summary_preserves_successful_command_evidence(self):
        self.make_independent_manifest()
        self.continue_fixture()
        self.use_empty_shell_results()
        self.add_native_commit_metadata()
        self.add_native_ui_preamble()
        self.save_native_events()
        self.select_proofs()
        original = self.results_path.read_bytes()
        self.exercise()
        self.assertEqual(self.results_path.read_bytes(), original)
        self.assertEqual((self.output / 'tester-results.json').read_bytes(), original)
        self.assertEqual(json.loads(original)['tests'], [])
        self.assertEqual(json.loads((self.output / 'released-upgrade.json').read_text())['result'], 'pass')
        for index, command in enumerate(self.results['commands']):
            for stream in ('stdout_log', 'stderr_log'):
                self.assertEqual((self.output / f'tester-package-{index}-{stream}.log').read_bytes(),
                                 (self.results_path.parent / command[stream]).read_bytes())

    def test_empty_shell_summary_rejects_invalid_commands_and_evidence(self):
        self.make_independent_manifest()
        self.continue_fixture()
        self.use_empty_shell_results()
        original_item = copy.deepcopy(self.after)
        streams = {self.results_path.parent / command[stream]:
                   (self.results_path.parent / command[stream]).read_bytes()
                   for command in self.results['commands'] for stream in ('stdout_log', 'stderr_log')}
        frozen = {path: path.read_bytes() for path in
                  (self.workspace / 'story.test.js', self.source / '.fluent/tester.yaml')}
        for change in ('missing-ci', 'missing-test', 'reordered', 'wrong-command', 'wrong-harness',
                       'nonzero-ci', 'nonzero-test', 'missing-exit', 'boolean-exit', 'error',
                       'failed-summary', 'failed-test', 'nonempty-tests', 'missing-summary',
                       'inconsistent-summary', 'stale-result', 'stale-task', 'foreign-task',
                       'unregistered-result', 'tampered-stdout', 'tampered-stderr', 'missing-stream',
                       'stale-selection', 'missing-stream-selection', 'duplicate-stream-selection',
                       'frozen-regression', 'frozen-config'):
            with self.subTest(change=change):
                for path, raw in {**streams, **frozen}.items(): path.write_bytes(raw)
                item, result = copy.deepcopy(original_item), copy.deepcopy(self.results)
                tester = next(t for t in item['attempts'][-1]['tasks'] if t['id'] == self.tester['id'])
                if change == 'missing-ci': result['commands'].pop(0)
                if change == 'missing-test': result['commands'].pop(1)
                if change == 'reordered': result['commands'].reverse()
                if change == 'wrong-command': result['commands'][1]['command'] = 'true'
                if change == 'wrong-harness': result['commands'][1]['test_harness'] = 'cargo-nextest'
                if change == 'nonzero-ci': result['commands'][0]['exit_code'] = 1
                if change == 'nonzero-test': result['commands'][1]['exit_code'] = 1
                if change == 'missing-exit': del result['commands'][1]['exit_code']
                if change == 'boolean-exit': result['commands'][1]['exit_code'] = False
                if change == 'error': result['error'] = {'kind': 'failed'}
                if change == 'failed-summary': result['summary']['fail'] = 1
                if change == 'failed-test': result['tests'] = [dict(id='regression', status='fail')]
                if change == 'nonempty-tests': result['tests'] = [dict(id='regression', status='pass')]
                if change == 'missing-summary': del result['summary']
                if change == 'inconsistent-summary': result['summary']['total'] = 1
                if change == 'stale-result': result['candidate_commit'] = self.initial
                if change == 'stale-task': tester['review_context']['candidate_commit'] = self.initial
                if change == 'foreign-task': tester['work_item_id'] = 'foreign'
                if change == 'unregistered-result':
                    item['attempts'][-1]['artifacts'] = [a for a in item['attempts'][-1]['artifacts']
                                                       if a['producer_id'] != tester['id']]
                self.save(self.item_path, item)
                self.save(self.results_path, result)
                self.select_proofs()
                if change == 'tampered-stdout':
                    self.save(self.results_path.parent / self.results['commands'][1]['stdout_log'], 'changed')
                if change == 'tampered-stderr':
                    self.save(self.results_path.parent / self.results['commands'][1]['stderr_log'], 'changed')
                if change == 'missing-stream':
                    (self.results_path.parent / self.results['commands'][1]['stdout_log']).unlink()
                if change == 'stale-selection': self.selection['tester']['results']['sha256'] = '0' * 64
                if change == 'missing-stream-selection': self.selection['tester']['artifacts'].pop()
                if change == 'duplicate-stream-selection':
                    self.selection['tester']['artifacts'].append(self.selection['tester']['artifacts'][0])
                if change == 'frozen-regression': self.save(self.workspace / 'story.test.js', '// disabled')
                if change == 'frozen-config': self.save(self.source / '.fluent/tester.yaml', 'commands: []')
                self.save(self.selection_path, self.selection)
                with self.assertRaises((RuntimeError, OSError)):
                    self.exercise()

    def select_proofs(self):
        self.selection = dict(schema_version=1, work=self.work, attempt=self.attempt_id,
            writer=dict(task_id=self.writer['id'], candidate=self.tip,
                session_id='native-session', generation='b' * 32,
                execution=dict(path=str(self.execution_path.relative_to(self.source)), sha256=upgrade.digest(self.execution_path)),
                receipt=dict(path=str((self.writer_log.parent / 'execution-receipt').relative_to(self.source)),
                             sha256=upgrade.digest(self.writer_log.parent / 'execution-receipt')),
                commands=[dict(command=c['command'], tool_use_id=f'tool-{index}',
                    artifacts=[dict(path=str(self.writer_log.relative_to(self.source)), sha256=upgrade.digest(self.writer_log))])
                          for index, c in enumerate(self.results['commands'])]),
            tester=dict(task_id=self.tester['id'], candidate=self.tip,
                results=dict(path=str(self.results_path.relative_to(self.source)), sha256=upgrade.digest(self.results_path)),
                artifacts=[dict(path=str((self.results_path.parent / c[stream]).relative_to(self.source)),
                                sha256=upgrade.digest(self.results_path.parent / c[stream]))
                           for c in self.results['commands'] for stream in ('stdout_log', 'stderr_log')]))
        self.save(self.selection_path, self.selection)
        self.args.package_proofs = self.selection_path

    def continue_fixture(self):
        self.assertFalse(self.results_path.exists())
        self.assertNotIn('package_proofs', json.loads(self.manifest_path.read_text()))
        receipt = upgrade.released_fixture(self.args, self.commands, self.candidate, self.binary)
        self.assertEqual(receipt['result'], 'continued')
        self.assertEqual(self.calls.count('public-resume'), 1)
        self.args.verify_only = True
        self.args.continuation_output = self.output
        self.select_proofs()
        # Verify into a separate receipt directory; retain the original invocation.
        self.output = self.root / 'verification'
        self.output.mkdir()
        self.commands.output = self.output

    def exercise(self):
        upgrade.released_fixture(self.args, self.commands, self.candidate, self.binary)

    def test_native_package_results_reject_masked_missing_and_foreign_evidence(self):
        self.continue_fixture()
        self.exercise()
        events = copy.deepcopy(self.native_events)
        for change in ('plain-text', 'whole-task', 'missing-ci', 'missing-test', 'pipeline',
                       'masked', 'background', 'quoted', 'failed', 'interrupted', 'missing-result',
                       'foreign-result', 'duplicate-result', 'foreign-session', 'foreign-cwd',
                       'changed-cwd', 'nested', 'unsupported', 'result-before-request', 'parallel', 'numeric-failure'):
            with self.subTest(change=change):
                self.native_events = copy.deepcopy(events)
                request, result = self.native_events[1:3]
                if change == 'whole-task': self.native_events = [events[0], events[-1]]
                if change == 'missing-ci': del self.native_events[1:3]
                if change == 'missing-test': del self.native_events[3:5]
                if change == 'pipeline': request['message']['content'][0]['input']['command'] += ' | tail -8'
                if change == 'masked': request['message']['content'][0]['input']['command'] += ' || true'
                if change == 'background': request['message']['content'][0]['input']['run_in_background'] = True
                if change == 'quoted': request['message']['content'][0]['input']['command'] = 'echo "' + upgrade.PACKAGE_COMMANDS[0] + '"'
                if change == 'failed': result['message']['content'][0]['is_error'] = True
                if change == 'interrupted': result['tool_use_result']['interrupted'] = True
                if change == 'missing-result': del self.native_events[2]
                if change == 'foreign-result': result['message']['content'][0]['tool_use_id'] = 'foreign'
                if change == 'duplicate-result': self.native_events.insert(3, copy.deepcopy(result))
                if change == 'foreign-session': result['session_id'] = 'other'
                if change == 'foreign-cwd': self.native_events[0]['cwd'] = str(self.root)
                if change == 'changed-cwd':
                    extra = copy.deepcopy(request)
                    extra['message']['content'][0].update(id='cd', input={'command': 'cd /tmp'})
                    self.native_events.insert(1, extra)
                if change == 'nested': request['parent_tool_use_id'] = 'subagent'
                if change == 'unsupported': result.pop('tool_use_result')
                if change == 'result-before-request': self.native_events[1:3] = [result, request]
                if change == 'parallel': self.native_events[2:4] = [events[3], result]
                if change == 'numeric-failure': result['tool_use_result']['exitCode'] = 1
                self.save_native_events()
                if change == 'plain-text': self.writer_log.write_text('\n'.join(upgrade.PACKAGE_COMMANDS) + '\nexit_code: 0\n')
                self.select_proofs()
                with self.assertRaises((RuntimeError, ValueError)):
                    self.exercise()
        self.native_events = events
        self.save_native_events()
        self.select_proofs()
        for change in ('generation', 'session', 'execution', 'receipt'):
            with self.subTest(binding=change):
                proof = copy.deepcopy(self.selection)
                if change == 'generation': proof['writer']['generation'] = 'c' * 32
                if change == 'session': proof['writer']['session_id'] = 'other'
                if change in ('execution', 'receipt'): proof['writer'][change]['sha256'] = '0' * 64
                self.save(self.selection_path, proof)
                with self.assertRaises(RuntimeError): self.exercise()

    def make_independent_manifest(self):
        self.recover_writer()
        self.before['learner_mode'] = self.after['learner_mode'] = 'no-expertise'
        session = '11111111-2222-4333-8444-555555555555'
        failure = dict(type='result', subtype='error_during_execution', is_error=True,
                       session_id=session, num_turns=0,
                       errors=[f'No conversation found with session ID: {session}'])
        self.save(self.failed_log, failure)
        self.before['attempts'][-1]['pause_record'] = {'task-id': self.writer['id'], 'candidate-commit': self.initial}
        self.before['attempts'][-1]['writer_runs'] = [dict(task_id=self.old_task['id'],
            candidate_commit=self.initial, provider='claude', session_id=session)]
        self.audit['before'] = copy.deepcopy(self.before['attempts'][-1])
        self.audit['files'][str(self.failed_log.relative_to(self.source))] = 'sha256:' + upgrade.digest(self.failed_log)
        # Recompute the public retry request after freezing the real failure.
        audit_digest = hashlib.sha256(json.dumps(self.audit, ensure_ascii=False, separators=(',', ':')).encode()).hexdigest()
        self.operation['input-digest'] = 'sha256:' + audit_digest
        identity = hashlib.sha256()
        for field in ('operation-kind', 'work-item-id', 'attempt-id', 'candidate-commit', 'task-id',
                      'evidence-digest', 'workspace-path', 'authorization', 'input-digest'):
            value = self.operation.get(field, '<none>').encode()
            identity.update(len(value).to_bytes(8, 'big')); identity.update(value)
        self.operation['operation-id'] = 'recovery-' + identity.hexdigest()
        self.writer['artifact_area']['path'] = self.area + '/writer-completion-retry-runs/' + self.operation['operation-id']
        self.writer_log = self.source / self.writer['artifact_area']['path'] / 'transcript.jsonl'
        self.save(self.audit_path, json.dumps(self.audit, ensure_ascii=False, indent=2))
        self.save(self.item_path, self.before)
        snapshot = Path(self.manifest['snapshot_root'])
        self.save(snapshot / 'original-item.json', self.before)
        self.save(snapshot / 'failure.json', failure)
        for name in ('config.yaml', 'tester.yaml'):
            self.save(snapshot / '.fluent' / name, (self.source / '.fluent' / name).read_text())
        self.git('tag', 'original-writer', self.initial)
        self.dynamic = dict(schema_version=1, scope='additional-genuine-060-writer-package-exercise',
            fixture_root=str(self.fixture), project='main', work=self.work, attempt=self.attempt_id,
            released=dict(path=self.manifest['released_binary'], sha256=upgrade.RELEASE_SHA256,
                          version='fluent 0.6.0 be19732c'),
            candidate=dict(commit=self.candidate, binary_path=str(self.binary),
                           binary_sha256=upgrade.digest(self.binary), version='fluent ' + upgrade.package_version() + ' ' + self.candidate[:8]),
            initial=dict(seed_commit=self.seed, writer_commit=self.initial, writer_task_id=self.old_task['id'],
                         writer_output=self.old_task['output'], assigned_workspace=str(self.workspace), host_tag='original-writer'),
            failure=dict(task_id=self.writer['id'], session_id=session,
                         artifact_path=str(self.failed_log.relative_to(self.source)),
                         artifact_sha256=upgrade.digest(self.failed_log), num_turns=0,
                         public_pause=self.before['attempts'][-1].get('pause_record')),
            configuration={str(p.relative_to(self.source)): upgrade.digest(p)
                           for p in (self.source / '.fluent').glob('*.yaml')},
            snapshot_root=str(snapshot), original_item='original-item.json',
            original_public_snapshot={name:upgrade.digest(snapshot / name) for name in ('original-item.json', 'failure.json', '.fluent/config.yaml', '.fluent/tester.yaml')},
            prerequisites=self.manifest['prerequisites'], prerequisites_sha256=self.manifest['prerequisites_sha256'],
            stage2_marker='stage-2', stage2_marker_sha256=self.manifest['stage2_marker_sha256'])
        for role in ('architecture', 'behaviors', 'documentation', 'skills', 'tests'):
            task = dict(id='review-' + role, role=role, kind='review', status='complete',
                        work_item_id=self.work, attempt_id=self.attempt_id,
                        review_context={'candidate_commit': self.tip},
                        artifact_area={'path': self.area + '/review-' + role})
            self.after['attempts'][-1]['tasks'].insert(-1, task)
            path = task['artifact_area']['path'] + '/review.md'
            self.save(self.source / path, 'Verdict: pass\n')
            self.after['attempts'][-1]['artifacts'].append(dict(producer_id=task['id'], path=path))
        self.save(self.manifest_path, self.dynamic)
        original_read = upgrade.read_command
        def read(argv, cwd):
            if len(argv) == 2 and argv[1] == 'version':
                if str(argv[0]) == str(getattr(self, 'historical_binary', None)):
                    return 'fluent 0.6.0 ' + upgrade.HISTORICAL_CANDIDATE[:8]
                return self.dynamic['released']['version'] if str(argv[0]) == self.dynamic['released']['path'] else self.dynamic['candidate']['version']
            return original_read(argv, cwd)
        patch = mock.patch.object(upgrade, 'read_command', side_effect=read)
        patch.start(); self.addCleanup(patch.stop)

    def test_package_successes_precede_the_actual_producing_commit(self):
        self.continue_fixture()
        events = copy.deepcopy(self.native_events)
        for change in ('missing-commit', 'missing-result', 'after-commit', 'overlap',
                       'foreign-commit', 'echoed-commit', 'failed-commit', 'foreign-directory',
                       'result-before-request', 'duplicate-commit', 'same-event'):
            with self.subTest(change=change):
                self.native_events = copy.deepcopy(events)
                request, result = self.native_events[5:7]
                if change == 'missing-commit': del self.native_events[5:7]
                if change == 'missing-result': del self.native_events[6]
                if change == 'after-commit': self.native_events[1:7] = [request, result, *events[1:5]]
                if change == 'overlap': self.native_events[4:6] = [request, events[4]]
                if change == 'foreign-commit': result['tool_use_result']['stdout'] = f'[main {self.initial[:7]}] Other\n'
                if change == 'echoed-commit': request['message']['content'][0]['input']['command'] = 'echo "git commit"'
                if change == 'failed-commit': result['message']['content'][0]['is_error'] = True
                if change == 'foreign-directory': request['message']['content'][0]['input']['command'] = 'git -C /tmp commit -m Other'
                if change == 'result-before-request': self.native_events[5:7] = [result, request]
                if change == 'duplicate-commit':
                    extra = copy.deepcopy([request, result])
                    extra[0]['message']['content'][0]['id'] = 'duplicate-commit'
                    extra[1]['message']['content'][0]['tool_use_id'] = 'duplicate-commit'
                    self.native_events[7:7] = extra
                if change == 'same-event':
                    self.native_events[4]['message']['content'].append(request['message']['content'][0])
                    del self.native_events[5]
                self.save_native_events()
                self.select_proofs()
                with self.assertRaises(RuntimeError): self.exercise()

    def test_package_workspace_rejects_dynamic_shell_context(self):
        self.continue_fixture()
        events = copy.deepcopy(self.native_events)
        for command in ('D=cd; $D /tmp', 'alias move=cd; move /tmp',
                        'move() { cd /tmp; }; move', 'eval "cd /tmp"',
                        'source /tmp/context', '. /tmp/context', '$(printf cd) /tmp',
                        'command cd /tmp', 'builtin cd /tmp', 'cd;/tmp',
                        'git status\nD=cd; $D /tmp'):
            with self.subTest(command=command):
                self.native_events = copy.deepcopy(events)
                self.native_events[1]['message']['content'][0]['input']['command'] = command
                self.save_native_events()
                self.select_proofs()
                with self.assertRaises(RuntimeError): self.exercise()

    def test_prior_bash_cd_keeps_later_package_cwd_unproven(self):
        self.continue_fixture()
        subdirectory = (self.source / self.writer['workspace_access']['writes'][0]['path'] / 'subdirectory').resolve()
        subdirectory.mkdir()
        prior = copy.deepcopy(self.native_events[1:3])
        prior[0]['message']['content'][0].update(
            id='prior-context', input={'command': 'cd ' + str(subdirectory)})
        prior[1]['message']['content'][0]['tool_use_id'] = 'prior-context'
        self.native_events[1:1] = prior
        self.save_native_events()
        self.select_proofs()
        with self.assertRaisesRegex(RuntimeError, 'assigned workspace'):
            self.exercise()

    def select_cwd_reset_observation(self):
        """Model the root observer's retained facts for this selected invocation."""
        import base64
        running = json.loads(self.execution_path.read_text())
        running['state'] = 'running'
        running.pop('settled_at')
        raw = json.dumps(running).encode()
        executable = self.root / 'observed-claude'
        executable.write_bytes(b'fixture executable')
        observed = dict(project=str(self.source), work=self.work,
            attempt=self.attempt_id, task=self.writer['id'],
            generation='b' * 32, pid=123, executable=str(executable),
            executable_sha256=upgrade.digest(executable),
            execution_record_base64=base64.b64encode(raw).decode(),
            execution_record_sha256=hashlib.sha256(raw).hexdigest(),
            selected_environment={'CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR': '1'},
            source='Darwin KERN_PROCARGS2, proc_pidpath, same-UID process group, unchanged execution bytes')
        path = self.root / 'cwd-reset-observation.json'
        self.save(path, observed)
        self.selection['writer']['cwd_reset_policy'] = dict(path=str(path), sha256=upgrade.digest(path))
        self.save(self.selection_path, self.selection)
        return path

    def test_bound_cwd_reset_policy_restores_proof_after_prior_cd(self):
        self.continue_fixture()
        prior = copy.deepcopy(self.native_events[1:3])
        prior[0]['message']['content'][0].update(id='prior-context',
            input={'command': 'cd subdirectory && pwd'})
        prior[1]['message']['content'][0]['tool_use_id'] = 'prior-context'
        self.native_events[1:1] = prior
        self.save_native_events()
        self.select_proofs()
        with self.assertRaisesRegex(RuntimeError, 'assigned workspace'):
            self.exercise()
        path = self.select_cwd_reset_observation()
        self.exercise()
        original = json.loads(path.read_text())
        for key, bad in (('generation', 'a' * 32), ('pid', 456),
                         ('task', 'foreign-task'), ('execution_record_sha256', '0' * 64)):
            with self.subTest(key=key):
                damaged = copy.deepcopy(original)
                damaged[key] = bad
                self.save(path, damaged)
                self.selection['writer']['cwd_reset_policy']['sha256'] = upgrade.digest(path)
                self.save(self.selection_path, self.selection)
                with self.assertRaises(RuntimeError): self.exercise()
        self.save(path, original)
        self.selection['writer']['cwd_reset_policy']['sha256'] = upgrade.digest(path)
        self.save(self.selection_path, self.selection)
        prior[1]['message']['content'][0]['is_error'] = True
        self.native_events[2] = prior[1]
        self.save_native_events()
        self.select_proofs()
        self.selection['writer']['cwd_reset_policy'] = dict(path=str(path), sha256=upgrade.digest(path))
        self.save(self.selection_path, self.selection)
        with self.assertRaises(RuntimeError): self.exercise()

    def test_bound_cwd_reset_policy_rejects_writer_writable_observation(self):
        self.continue_fixture()
        prior = copy.deepcopy(self.native_events[1:3])
        prior[0]['message']['content'][0].update(id='prior-context',
            input={'command': 'cd subdirectory && pwd'})
        prior[1]['message']['content'][0]['tool_use_id'] = 'prior-context'
        self.native_events[1:1] = prior
        self.save_native_events()
        self.select_proofs()
        retained = self.select_cwd_reset_observation()
        writable = self.workspace / 'cwd-reset-observation.json'
        writable.write_bytes(retained.read_bytes())
        self.selection['writer']['cwd_reset_policy'] = dict(
            path=str(writable), sha256=upgrade.digest(writable))
        self.save(self.selection_path, self.selection)
        with self.assertRaisesRegex(RuntimeError, 'Writer writable root'):
            self.exercise()

    def test_successful_explicit_workspace_cd_restores_package_proof(self):
        self.continue_fixture()
        subdirectory = (self.source / self.writer['workspace_access']['writes'][0]['path'] / 'subdirectory').resolve()
        subdirectory.mkdir()
        prior = copy.deepcopy(self.native_events[1:3])
        prior[0]['message']['content'][0].update(
            id='prior-context', input={'command': 'cd ' + str(subdirectory)})
        prior[1]['message']['content'][0]['tool_use_id'] = 'prior-context'
        reset = copy.deepcopy(self.native_events[1:3])
        reset[0]['message']['content'][0].update(
            id='workspace-reset', input={'command': 'cd ' + str((self.source / self.writer['workspace_access']['writes'][0]['path']).resolve())})
        reset[1]['message']['content'][0]['tool_use_id'] = 'workspace-reset'
        self.native_events[1:1] = prior + reset
        self.save_native_events()
        self.select_proofs()
        self.exercise()
        reset[1]['message']['content'][0]['is_error'] = True
        self.native_events[4] = reset[1]
        self.save_native_events()
        self.select_proofs()
        with self.assertRaisesRegex(RuntimeError, 'assigned workspace'):
            self.exercise()

    def test_native_success_preserves_only_observed_numeric_exits(self):
        self.continue_fixture()
        self.exercise()
        for check in self.selection['writer']['commands']:
            check['exit_code'] = 0
        self.save(self.selection_path, self.selection)
        with self.assertRaisesRegex(RuntimeError, 'numeric exit is absent'):
            self.exercise()
        for position in (2, 4):
            self.native_events[position]['tool_use_result']['exitCode'] = 0
        self.save_native_events()
        self.select_proofs()
        for check in self.selection['writer']['commands']:
            check['exit_code'] = 0
        self.save(self.selection_path, self.selection)
        self.exercise()
        del self.native_events[2]['message']['content'][0]['is_error']
        self.save_native_events()
        self.select_proofs()
        with self.assertRaisesRegex(RuntimeError, 'native result failed'):
            self.exercise()

    def test_independent_freezes_first_regression_tree_in_assigned_workspace(self):
        self.make_independent_manifest()
        self.continue_fixture()
        self.exercise()
        self.assertNotEqual(self.source, self.workspace)
        self.assertNotEqual(self.seed, self.initial)
        self.assertNotIn('story.test.js', self.git('ls-tree', '-r', '--name-only', self.seed))
        frozen = ('package.json', 'package-lock.json', 'vendor/example/index.js',
                  'AGENTS.md', '.fluent/config.yaml', '.fluent/tester.yaml', 'story.test.js')
        source_bytes = {name: (self.source / name).read_bytes() for name in frozen}
        for change in (*frozen, 'removed', 'renamed', 'new', 'mode', 'dirty', 'staged', 'untracked', 'wrong-tip'):
            with self.subTest(change=change):
                def git(*args):
                    return subprocess.check_output(['git', *args], cwd=self.workspace,
                                                   stderr=subprocess.PIPE, text=True).strip()
                try:
                    if change in frozen:
                        (self.workspace / change).write_text('{"scripts":{"test":"true"}}' if change == 'package.json' else 'tampered\n')
                    if change == 'removed': (self.workspace / 'story.test.js').unlink()
                    if change == 'renamed': (self.workspace / 'story.test.js').rename(self.workspace / 'renamed.test.js')
                    if change in ('new', 'untracked'): (self.workspace / 'new-input').write_text('new\n')
                    if change == 'mode': (self.workspace / 'story.test.js').chmod(0o755)
                    if change in ('dirty', 'staged'): (self.workspace / 'story.js').write_text('uncommitted\n')
                    if change == 'wrong-tip': git('checkout', '--detach', self.initial)
                    elif change != 'untracked' and change != 'dirty':
                        git('add', '-A')
                        if change != 'staged': git('commit', '-qm', 'Alter candidate')
                    tip = git('rev-parse', 'HEAD') if change != 'wrong-tip' else self.tip
                    item = copy.deepcopy(self.after)
                    attempt = item['attempts'][-1]
                    next(t for t in attempt['tasks'] if t['id'] == self.writer['id'])['output']['commit'] = tip
                    for t in attempt['tasks']:
                        if t.get('kind') == 'review': t['review_context']['candidate_commit'] = tip
                    with self.assertRaisesRegex(RuntimeError, 'frozen|workspace'):
                        upgrade.require_independent_completion(self.source, item, attempt, self.dynamic, tip)
                    self.assertEqual(source_bytes, {name: (self.source / name).read_bytes() for name in frozen})
                finally:
                    git('reset', '--hard', self.tip)
                    for name in ('new-input', 'renamed.test.js'):
                        (self.workspace / name).unlink(missing_ok=True)

    def test_independent_accepts_carried_forward_skills_review(self):
        self.make_independent_manifest()
        attempt = self.after['attempts'][-1]
        (self.workspace / 'README.md').write_text('Updated example story\n')
        subprocess.check_call(['git', 'add', 'README.md'], cwd=self.workspace)
        subprocess.check_call(['git', 'commit', '-qm', 'Update example documentation'], cwd=self.workspace)
        tip = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=self.workspace, text=True).strip()
        writer = copy.deepcopy(self.writer)
        writer['id'] = 'write-documentation'
        writer['output']['commit'] = tip
        attempt['tasks'].insert(-1, writer)
        documentation = next(task for task in attempt['tasks']
                             if task.get('kind') == 'review' and task.get('role') == 'documentation')
        documentation['review_context']['candidate_commit'] = tip
        attempt['tasks'].insert(-1, copy.deepcopy(documentation))
        attempt['tasks'][-2]['id'] = 'review-documentation-final'
        attempt['tasks'][-2]['artifact_area']['path'] = self.area + '/review-documentation-final'
        review_path = self.area + '/review-documentation-final/review.md'
        self.save(self.source / review_path, 'Verdict: pass\n')
        attempt['artifacts'].append(dict(producer_id='review-documentation-final', path=review_path))
        upgrade.require_independent_completion(self.source, self.after, attempt, self.dynamic, tip)
        without_documentation = copy.deepcopy(attempt)
        without_documentation['tasks'] = [task for task in without_documentation['tasks']
                                          if task['id'] != 'review-documentation-final']
        with self.assertRaisesRegex(RuntimeError, 'ordinary review is missing or stale'):
            upgrade.require_independent_completion(self.source, self.after,
                                                   without_documentation, self.dynamic, tip)
        stale_skills = copy.deepcopy(attempt)
        next(task for task in stale_skills['tasks'] if task.get('role') == 'skills')[
            'review_context']['candidate_commit'] = self.initial
        with self.assertRaisesRegex(RuntimeError, 'ordinary review is missing or stale'):
            upgrade.require_independent_completion(self.source, self.after,
                                                   stale_skills, self.dynamic, tip)

    def test_independent_exercise_uses_its_own_failure_and_public_retry(self):
        self.make_independent_manifest()
        self.continue_fixture()
        self.exercise()
        receipt = json.loads((self.output / 'released-upgrade.json').read_text())
        self.assertEqual(receipt['result'], 'pass')
        self.assertEqual(receipt['missing_session'], self.dynamic['failure']['session_id'])
        self.assertNotEqual(receipt['missing_session'], upgrade.MISSING_SESSION)
        for change in ('manifest', 'release', 'candidate', 'failure', 'configuration', 'history', 'reservation'):
            with self.subTest(change=change):
                original = copy.deepcopy(self.dynamic)
                path, saved = None, None
                if change == 'manifest': self.dynamic['initial']['host_tag'] = 'other'
                if change == 'release': self.dynamic['released']['sha256'] = '0' * 64
                if change == 'candidate': self.dynamic['candidate']['commit'] = 'f' * 40
                if change == 'failure': self.dynamic['failure']['session_id'] = upgrade.MISSING_SESSION
                if change == 'configuration': path = self.source / '.fluent/config.yaml'
                if change == 'history': path = self.old_artifact
                if path:
                    saved = path.read_bytes(); path.write_text('changed')
                if change == 'reservation':
                    path = self.source / self.area / 'learner/run-3/execution.json'
                    self.save(path, dict(state='launching', leader_pid=0, process_group_id=0))
                    item = json.loads(self.item_path.read_text())
                    item['attempts'][-1]['artifacts'].append(dict(producer_id='local-execution', path=str(path.relative_to(self.source))))
                    self.save(self.item_path, item)
                self.save(self.manifest_path, self.dynamic)
                with self.assertRaises((RuntimeError, OSError, subprocess.CalledProcessError)):
                    self.exercise()
                self.dynamic = original
                self.save(self.manifest_path, original)
                if saved: path.write_bytes(saved)
                self.save(self.item_path, self.after)

    def test_independent_rejects_fabricated_failure_and_incomplete_completion(self):
        self.make_independent_manifest()
        original = copy.deepcopy(self.dynamic)
        for change in ('failure-task', 'failure-turns', 'failure-session', 'initial-output', 'initial-workspace',
                       'initial-tag', 'release-version', 'binary-hash', 'snapshot'):
            with self.subTest(change=change):
                manifest = copy.deepcopy(original)
                if change == 'failure-task': manifest['failure']['task_id'] = self.old_task['id']
                if change == 'failure-turns': manifest['failure']['num_turns'] = 1
                if change == 'failure-session': manifest['failure']['session_id'] = 'fabricated'
                if change == 'initial-output': manifest['initial']['writer_output']['commit'] = self.tip
                if change == 'initial-workspace': manifest['initial']['assigned_workspace'] = str(self.root)
                if change == 'initial-tag': manifest['initial']['host_tag'] = 'missing-tag'
                if change == 'release-version': manifest['released']['version'] = 'fluent 0.6.1'
                if change == 'binary-hash': manifest['candidate']['binary_sha256'] = '0' * 64
                if change == 'snapshot': manifest['original_public_snapshot']['original-item.json'] = '0' * 64
                self.save(self.manifest_path, manifest)
                with self.assertRaises((RuntimeError, ValueError, subprocess.CalledProcessError)):
                    self.exercise()
        self.assertNotIn('public-resume', self.calls)
        self.save(self.manifest_path, original)
        self.continue_fixture()
        for change in ('review', 'review-candidate', 'learning', 'policy', 'recovery', 'missing-reservation', 'later-writer'):
            with self.subTest(completion=change):
                item = copy.deepcopy(self.after)
                attempt = item['attempts'][-1]
                if change == 'review': attempt['tasks'] = [t for t in attempt['tasks'] if t.get('role') != 'skills']
                if change == 'review-candidate':
                    next(t for t in attempt['tasks'] if t.get('role') == 'skills')['review_context']['candidate_commit'] = self.initial
                if change == 'learning': attempt['learning']['status'] = 'failed'
                if change == 'policy': item['learner_mode'] = 'capture'
                if change == 'recovery': attempt['recovery_operations'] = []
                if change == 'missing-reservation': attempt['artifacts'].append(dict(producer_id='local-execution', path=self.area + '/learner/run-3/execution.json'))
                if change == 'later-writer': attempt['tasks'].append(dict(self.writer, id='later-writer', output={'commit': self.initial}))
                self.save(self.item_path, item)
                with self.assertRaises((RuntimeError, OSError)):
                    self.exercise()
        self.save(self.item_path, self.after)
        self.args.verify_only = False
        with self.assertRaisesRegex(RuntimeError, 'completed independent exercise'):
            self.exercise()
        self.assertEqual(self.calls.count('public-resume'), 1)

    def make_historical_envelope(self):
        self.historical_fixture = self.root / 'historical/real-060-node-claude-upgrade'
        source = self.historical_fixture / 'main'
        subprocess.run(['git', 'clone', '-q', '--no-hardlinks', str(self.source), str(source)], check=True)
        def relocate(text):
            return text.replace(str(self.fixture), str(self.historical_fixture)).replace(self.work, 'historical-work')
        for path in (self.source / '.fluent').rglob('*'):
            if path.is_file():
                self.save(source / relocate(str(path.relative_to(self.source))), relocate(path.read_text()))
        item = json.loads(relocate(json.dumps(self.after)))
        self.historical_item = source / '.fluent/work/items/historical-work.json'
        for run in (2, 3):
            path = f'.fluent/work/artifacts/historical-work/{self.attempt_id}/learner/run-{run}/execution.json'
            self.save(source / path, dict(schema_version=2, state='launching', leader_pid=0, process_group_id=0,
                launch_generation=str(run) * 32, attempt_owner=dict(project_root=str(source),
                work_item_id='historical-work', attempt_id=self.attempt_id)))
            item['attempts'][-1]['artifacts'].append(dict(producer_id='local-execution', path=path))
        self.save(self.historical_item, item)
        self.save(self.historical_fixture / 'stage-2', 'advanced by fixture owner')
        failure = source / 'original-failure.json'
        self.save(failure, dict(session_id=upgrade.MISSING_SESSION, num_turns=0, errors=['No conversation found']))
        snapshot = self.root / 'historical-snapshot'
        snapshot.mkdir()
        for index in range(26): (snapshot / str(index)).write_text(str(index))
        manifest = dict(self.manifest, fixture=str(self.historical_fixture), work='historical-work',
            original_failure='main/original-failure.json', original_failure_sha256=upgrade.digest(failure),
            snapshot_root=str(snapshot), snapshot_files={p.name:upgrade.digest(p) for p in snapshot.iterdir()})
        configuration = {str(p.relative_to(source)):upgrade.digest(p) for p in (source / '.fluent').glob('*.yaml')}
        manifest['config_sha256'] = configuration['.fluent/config.yaml']
        self.historical_binary = self.root / 'historical-binary'
        self.historical_binary.write_text('original historical executable')
        binary = dict(candidate=upgrade.HISTORICAL_CANDIDATE, path=str(self.historical_binary),
                      sha256=upgrade.digest(self.historical_binary), version='fluent 0.6.0 ' + upgrade.HISTORICAL_CANDIDATE[:8])
        history = upgrade.retained_history(source, item, 'historical-work', self.attempt_id)
        files = {}
        prefix = '/test/6167-continuation-host2/host-output/'
        def add(path, value): files[path] = json.dumps(value).encode()
        add(prefix + 'frozen-manifest.json', manifest)
        add(prefix + 'fixture-before.json', item)
        add(prefix + 'retained-history.json', history)
        add(prefix + 'continuation.json', dict(candidate=upgrade.HISTORICAL_CANDIDATE, invoked=True,
            binary_sha256=binary['sha256'], fixture=str(self.historical_fixture), work='historical-work',
            attempt=self.attempt_id, configuration=configuration,
            files={Path(p).name:hashlib.sha256(v).hexdigest() for p,v in files.items()}))
        add(prefix + 'binary.json', binary)
        add(prefix + 'fixture-after.json', item)
        export = dict(candidate=self.tip, manifest=[])
        for path in source.rglob('*'):
            if path.is_file() and '.git' not in path.parts:
                export['manifest'].append(dict(source=str(path), bytes=path.stat().st_size, sha256=upgrade.digest(path)))
        add('/test/fluent-row4-public-completed-proof-6167/manifest.json', export)
        ownership = {}
        for attempt in item['attempts']:
            owner = upgrade.OwnedAttempt([self.binary, 'attempt', 'run', 'historical-work', attempt['id']], source, {})
            snapshot = owner.snapshot(item)
            ownership[attempt['id']] = dict(executions=snapshot['executions'], setup=snapshot['setup'])
        add('/test/6167-continuation-host2/ownership-diagnostic.json', dict(attempts=ownership))
        data = json.dumps(dict(manifest=[dict(path=p, bytes=len(v), sha256=hashlib.sha256(v).hexdigest())
                                        for p,v in files.items()],
                               blobs_base64={hashlib.sha256(v).hexdigest():base64.b64encode(v).decode()
                                             for v in files.values()})).encode()
        compressed = lzma.compress(data)
        payload = dict(archive=dict(data=base64.b64encode(compressed).decode(),
            xz_sha256=hashlib.sha256(compressed).hexdigest(), uncompressed_bytes=len(data),
            uncompressed_sha256=hashlib.sha256(data).hexdigest()))
        self.args.historical_evidence = self.root / 'historical-envelope.json'
        self.save(self.args.historical_evidence, dict(check=upgrade.HOST_CHECKS['released'][1], result='fail', output=json.dumps(payload)))
        patch = mock.patch.object(upgrade, 'HISTORICAL_ENVELOPE_SHA256', upgrade.digest(self.args.historical_evidence))
        patch.start(); self.addCleanup(patch.stop)
        patch = mock.patch.object(upgrade, 'show', side_effect=lambda binary, cwd, work:
                                  json.loads((Path(cwd) / f'.fluent/work/items/{work}.json').read_text()))
        patch.start(); self.addCleanup(patch.stop)

    def test_aggregate_keeps_historical_gaps_separate_from_current_acceptance(self):
        self.make_independent_manifest()
        self.continue_fixture()
        self.make_historical_envelope()
        receipt = upgrade.released_aggregate(self.args, self.commands, self.candidate, self.binary)
        self.assertEqual(receipt['result'], 'pass')
        self.assertEqual(receipt['historical']['result'], 'retained-with-gaps')
        self.assertEqual(receipt['historical']['candidate'], upgrade.HISTORICAL_CANDIDATE)
        self.assertEqual(receipt['current']['candidate'], self.candidate)
        self.assertEqual(len(receipt['historical']['nonterminal_unused_reservations']), 2)
        self.assertEqual(self.calls.count('public-resume'), 1)
        for change in ('envelope', 'historical-binary', 'historical-record', 'historical-reservation', 'graft', 'same-fixture', 'same-session', 'same-work'):
            with self.subTest(change=change):
                path = None
                manifest = copy.deepcopy(self.dynamic)
                if change == 'envelope': path = self.args.historical_evidence
                if change == 'historical-binary': path = self.historical_binary
                if change == 'historical-record': path = self.historical_item
                if change == 'historical-reservation': path = self.historical_fixture / 'main/.fluent/work/artifacts/historical-work' / self.attempt_id / 'learner/run-3/execution.json'
                if change == 'graft':
                    selection = copy.deepcopy(self.selection)
                    selection['writer']['commands'][0]['artifacts'][0]['path'] = str(self.historical_fixture / 'main' / self.writer_log.relative_to(self.source))
                    self.save(self.selection_path, selection)
                if change == 'same-fixture': self.args.fixture = self.historical_fixture
                if change == 'same-session': manifest['failure']['session_id'] = upgrade.MISSING_SESSION
                if change == 'same-work': manifest['work'] = 'historical-work'
                if path:
                    saved = path.read_bytes(); path.write_text('{}')
                self.save(self.manifest_path, manifest)
                with self.assertRaises((RuntimeError, ValueError, OSError)):
                    upgrade.released_aggregate(self.args, self.commands, self.candidate, self.binary)
                if path: path.write_bytes(saved)
                self.args.fixture = self.fixture
                self.save(self.manifest_path, self.dynamic)
                self.save(self.selection_path, self.selection)

    def test_history_preserves_registered_writer_commit_and_file_evidence(self):
        history = upgrade.retained_history(self.source, self.before, self.work, self.attempt_id)
        self.assertNotIn(self.initial, history['files'])
        # Persist the receipt just as the continuation and verify-only paths do.
        history = json.loads(json.dumps(history))
        upgrade.verify_retained_history(self.source, self.after, history)
        self.assertFalse((self.source / self.initial).exists())
        self.assertEqual(history['files'][str(self.old_log.relative_to(self.source))], upgrade.digest(self.old_log))

    def test_history_rejects_invalid_commit_producer_and_output(self):
        for change in ('producer', 'output', 'kind', 'status', 'attempt', 'work', 'ambiguous'):
            with self.subTest(change=change):
                before = copy.deepcopy(self.before)
                attempt = before['attempts'][-1]
                task = attempt['tasks'][0]
                if change == 'producer': attempt['artifacts'][0]['producer_id'] = 'unknown'
                if change == 'output': task['output']['commit'] = self.tip
                if change == 'kind': task['kind'] = 'review'
                if change == 'status': task['status'] = 'failed'
                if change == 'attempt': task['attempt_id'] = 'attempt-1'
                if change == 'work': task['work_item_id'] = 'other'
                if change == 'ambiguous': attempt['tasks'].append(copy.deepcopy(task))
                # Even a SHA-named file cannot substitute for a registered object.
                (self.source / self.initial).write_text('not a Git object')
                with self.assertRaisesRegex(RuntimeError, 'registered Writer commit'):
                    upgrade.retained_history(self.source, before, self.work, self.attempt_id)

    def test_history_requires_commit_objects_and_recorded_base_ancestry(self):
        blob = self.git('rev-parse', 'HEAD:package.json')
        self.git('tag', '-am', 'Retain a tag, not a commit', 'tagged')
        tag = self.git('rev-parse', 'tagged')
        for commit, base in [('f' * 40, self.initial), (blob, self.initial),
                             (tag, self.initial), (self.initial, 'f' * 40),
                             (self.initial, self.tip)]:
            with self.subTest(commit=commit, base=base):
                before = copy.deepcopy(self.before)
                attempt = before['attempts'][-1]
                attempt['artifacts'][0]['path'] = commit
                attempt['tasks'][0]['output'].update(commit=commit, base_commit=base)
                with self.assertRaisesRegex(RuntimeError, 'registered Writer commit'):
                    upgrade.retained_history(self.source, before, self.work, self.attempt_id)

    def test_history_accepts_unlanded_writer_without_using_source_head(self):
        before = copy.deepcopy(self.before)
        attempt = before['attempts'][-1]
        attempt['artifacts'][0]['path'] = self.tip
        attempt['tasks'][0]['output']['commit'] = self.tip
        self.git('checkout', '-q', '--detach', self.initial)
        history = upgrade.retained_history(self.source, before, self.work, self.attempt_id)
        upgrade.verify_retained_history(self.source, before, history)
        self.assertEqual(self.git('rev-parse', 'HEAD'), self.initial)

    def test_history_rechecks_actual_objects_after_continuation(self):
        history = upgrade.retained_history(self.source, self.before, self.work, self.attempt_id)
        object_path = self.source / '.git/objects' / self.initial[:2] / self.initial[2:]
        object_path.unlink()
        with self.assertRaisesRegex(RuntimeError, 'registered Writer commit object'):
            upgrade.verify_retained_history(self.source, self.after, history)

    def test_history_verifies_registered_reference_after_continuation(self):
        history = upgrade.retained_history(self.source, self.before, self.work, self.attempt_id)
        for change in ('removed', 'producer', 'path', 'duplicate', 'attempt'):
            with self.subTest(change=change):
                after = copy.deepcopy(self.after)
                artifacts = after['attempts'][-1]['artifacts']
                if change == 'removed': artifacts.pop(0)
                if change == 'producer': artifacts[0]['producer_id'] = self.writer['id']
                if change == 'path': artifacts[0]['path'] = self.tip
                if change == 'duplicate': artifacts.append(copy.deepcopy(artifacts[0]))
                if change == 'attempt': after['attempts'][0]['artifacts'] = [artifacts.pop(0)]
                with self.assertRaisesRegex(RuntimeError, 'retained registered artifact changed'):
                    upgrade.verify_retained_history(self.source, after, history)

    def test_history_keeps_file_scope_and_digest_checks(self):
        relative = str(self.old_artifact.relative_to(self.source))
        self.before['attempts'][-1]['artifacts'].append(dict(producer_id=self.old_task['id'], path=relative))
        history = upgrade.retained_history(self.source, self.before, self.work, self.attempt_id)
        after = copy.deepcopy(self.before)
        self.old_artifact.write_text('changed')
        with self.assertRaisesRegex(RuntimeError, 'retained artifact changed'):
            upgrade.verify_retained_history(self.source, after, history)
        for path in ('.fluent/work/artifacts/other/attempt-2/evidence.json',
                     self.area + '/claude-home/evidence.json', self.area + '/symlink.json',
                     self.area + '/../evidence.json', 'f' * 40):
            with self.subTest(path=path):
                target = self.source / path
                target.parent.mkdir(parents=True, exist_ok=True)
                if 'symlink' in path: target.symlink_to(self.old_artifact)
                else: target.write_text('not admissible evidence')
                before = copy.deepcopy(self.before)
                before['attempts'][-1]['artifacts'][-1]['path'] = path
                with self.assertRaises(RuntimeError):
                    upgrade.retained_history(self.source, before, self.work, self.attempt_id)

    def recover_writer(self, kind='claude-session-retry'):
        """Retain the failed Task and apply the public prepared_task assignment."""
        version, source, directory = {
            'claude-session-retry': (3, 'missing-claude-conversation', 'claude-session-retries'),
            'writer-completion-context-retry': (1, None, 'writer-completion-context-retries'),
            'writer-completion-no-change-retry': (2, 'invalid-completion', 'writer-completion-no-change-retries'),
        }[kind]
        self.writer.update(workspace_access=dict(reads=[], writes=[dict(id='candidate', path='../candidate')]),
                           writer_correction_assignment=dict(source_candidate_commit=self.initial), input_artifacts=[])
        failed = copy.deepcopy(self.writer)
        failed.update(status='failed', output=None)
        self.failed_log = self.source / failed['artifact_area']['path'] / 'transcript.jsonl'
        self.save(self.failed_log, 'original failed Writer output\n')
        self.before['attempts'][-1]['tasks'].append(failed)
        self.save(self.item_path, self.before)
        self.audit = dict(schema_version=version)
        if source: self.audit['source'] = source
        self.audit.update(before=copy.deepcopy(self.before['attempts'][-1]), task_id=failed['id'],
                          files={str(self.failed_log.relative_to(self.source)): 'sha256:' + upgrade.digest(self.failed_log)},
                          resolution='Resume the exact failed Writer')
        audit_digest = hashlib.sha256(json.dumps(self.audit, ensure_ascii=False, separators=(',', ':')).encode()).hexdigest()
        self.operation = {'schema-version': 1, 'operation-kind': kind, 'work-item-id': self.work,
                          'attempt-id': self.attempt_id, 'candidate-commit': self.initial, 'task-id': failed['id'],
                          'workspace-path': '../candidate', 'authorization': self.audit['resolution'],
                          'authorization-digest': 'sha256:' + hashlib.sha256(self.audit['resolution'].encode()).hexdigest(),
                          'input-digest': 'sha256:' + audit_digest,
                          'result': 'completion-retry' if version == 2 else 'applied'}
        identity = hashlib.sha256()
        for field in ('operation-kind', 'work-item-id', 'attempt-id', 'candidate-commit', 'task-id',
                      'evidence-digest', 'workspace-path', 'authorization', 'input-digest'):
            value = self.operation.get(field, '<none>').encode()
            identity.update(len(value).to_bytes(8, 'big')); identity.update(value)
        self.operation['operation-id'] = 'recovery-' + identity.hexdigest()
        self.audit_ref = dict(producer_id=kind, path=f'{self.area}/{directory}/{failed["id"]}/recovery.json')
        self.audit_path = self.source / self.audit_ref['path']
        self.save(self.audit_path, json.dumps(self.audit, ensure_ascii=False, indent=2))
        self.writer['artifact_area']['path'] = self.area + '/writer-completion-retry-runs/' + self.operation['operation-id']
        self.writer['input_artifacts'].append(self.audit_ref)
        self.writer_log = self.source / self.writer['artifact_area']['path'] / 'transcript.jsonl'
        attempt = self.after['attempts'][-1]
        attempt['recovery_operations'] = [self.operation]
        attempt['artifacts'].append(self.audit_ref)

    def test_recovered_writer_packages_use_exact_approved_assignment(self):
        self.recover_writer()
        original_failure = self.failed_log.read_bytes()
        self.continue_fixture()
        self.exercise()
        self.exercise()
        self.assertEqual(self.calls.count('public-resume'), 1)
        self.assertEqual(self.failed_log.read_bytes(), original_failure)
        self.assertEqual(json.loads((self.output / 'released-upgrade.json').read_text())['result'], 'pass')
        self.assertEqual((self.output / 'writer-package-0-0.log').read_bytes(), self.writer_log.read_bytes())

    def test_context_retry_packages_use_their_own_audit(self):
        self.recover_writer('writer-completion-context-retry')
        self.continue_fixture()
        self.exercise()

    def test_completion_retry_packages_use_their_own_audit(self):
        self.recover_writer('writer-completion-no-change-retry')
        self.continue_fixture()
        self.exercise()

    def test_recovered_writer_retains_original_failed_artifacts(self):
        self.recover_writer()
        self.continue_fixture()
        self.exercise()
        self.failed_log.write_text('changed failed Writer output\n')
        with self.assertRaisesRegex(RuntimeError, 'retained artifact changed'):
            self.exercise()

    def test_recovered_writer_rejects_changed_assignment_operation_and_audit(self):
        self.recover_writer()
        self.continue_fixture()
        self.exercise()
        original = copy.deepcopy(self.after)
        for change in ('area', 'canonical-area', 'area-traversal', 'area-absolute', 'area-work', 'area-attempt',
                       'assignment', 'workspace', 'task-work', 'task-attempt',
                       'operation-id', 'operation-kind', 'operation-task', 'operation-work', 'operation-attempt',
                       'operation-candidate', 'operation-input', 'operation-authorization', 'operation-result',
                       'operation-schema', 'operation-authorization-digest', 'operation-evidence',
                       'missing-operation', 'duplicate-operation', 'audit-input', 'audit-input-path', 'audit-registration',
                       'audit-bytes', 'audit-whitespace', 'audit-task', 'audit-work', 'audit-attempt', 'audit-source', 'audit-candidate'):
            with self.subTest(change=change):
                item, audit = copy.deepcopy(original), copy.deepcopy(self.audit)
                attempt = item['attempts'][-1]
                writer = attempt['tasks'][-2]
                operation = attempt['recovery_operations'][0]
                if change == 'area': writer['artifact_area']['path'] += '-sibling'
                if change == 'canonical-area': writer['artifact_area']['path'] = self.area + '/' + writer['id']
                if change == 'area-traversal': writer['artifact_area']['path'] += '/../sibling'
                if change == 'area-absolute': writer['artifact_area']['path'] = str(self.source / writer['artifact_area']['path'])
                if change == 'area-work': writer['artifact_area']['path'] = writer['artifact_area']['path'].replace(self.work, 'foreign')
                if change == 'area-attempt': writer['artifact_area']['path'] = writer['artifact_area']['path'].replace(self.attempt_id, 'attempt-foreign')
                if change == 'assignment': writer['writer_correction_assignment']['source_candidate_commit'] = self.tip
                if change == 'workspace': writer['workspace_access']['writes'][0]['path'] = '../foreign'
                if change == 'task-work': writer['work_item_id'] = 'foreign'
                if change == 'task-attempt': writer['attempt_id'] = 'attempt-foreign'
                for suffix, field in (('id', 'operation-id'), ('kind', 'operation-kind'), ('task', 'task-id'),
                                      ('work', 'work-item-id'), ('attempt', 'attempt-id'), ('candidate', 'candidate-commit'),
                                      ('input', 'input-digest'), ('authorization', 'authorization'), ('result', 'result'),
                                      ('schema', 'schema-version'), ('authorization-digest', 'authorization-digest'),
                                      ('evidence', 'evidence-digest')):
                    if change == 'operation-' + suffix: operation[field] = 'changed'
                if change == 'missing-operation': attempt['recovery_operations'] = []
                if change == 'duplicate-operation': attempt['recovery_operations'].append(copy.deepcopy(operation))
                if change == 'audit-input': writer['input_artifacts'] = []
                if change == 'audit-input-path': writer['input_artifacts'][-1]['path'] += '/../foreign.json'
                if change == 'audit-registration': attempt['artifacts'].remove(self.audit_ref)
                if change == 'audit-bytes': audit['resolution'] += ' changed'
                if change == 'audit-task': audit['task_id'] = 'foreign'
                if change == 'audit-work': audit['before']['work_item_id'] = 'foreign'
                if change == 'audit-attempt': audit['before']['id'] = 'attempt-foreign'
                if change == 'audit-source': audit['source'] = 'invalid-completion'
                if change == 'audit-candidate': audit['before']['tasks'][0]['output']['commit'] = self.tip
                self.save(self.item_path, item)
                self.save(self.audit_path, json.dumps(audit, ensure_ascii=False, indent=2) +
                          ('\n' if change == 'audit-whitespace' else ''))
                with self.assertRaisesRegex(RuntimeError, 'package|recovery|retry|snapshot path escapes'):
                    self.exercise()
        self.save(self.item_path, original)
        self.save(self.audit_path, json.dumps(self.audit, ensure_ascii=False, indent=2))

    def test_recovered_writer_confines_evidence_and_rejects_symlinks(self):
        self.recover_writer()
        self.continue_fixture()
        self.exercise()
        original = copy.deepcopy(self.selection)
        for relative in (self.area + '/writer-completion-retry-runs/sibling/transcript.jsonl',
                         self.area.replace(self.work, 'foreign') + '/write/transcript.jsonl',
                         self.area.replace(self.attempt_id, 'attempt-foreign') + '/write/transcript.jsonl',
                         self.writer['artifact_area']['path'] + '/../sibling/transcript.jsonl',
                         str(self.failed_log.relative_to(self.source))):
            with self.subTest(path=relative):
                if '..' not in Path(relative).parts and relative != str(self.failed_log.relative_to(self.source)):
                    self.save(self.source / relative, self.writer_log.read_text())
                selected = copy.deepcopy(original)
                selected['writer']['commands'][0]['artifacts'][0] = dict(
                    path=relative, sha256=upgrade.digest(self.source / relative))
                self.save(self.selection_path, selected)
                with self.assertRaisesRegex(RuntimeError, 'outside|escapes'):
                    self.exercise()
        self.save(self.selection_path, original)
        for path in (self.writer_log, self.writer_log.parent, self.audit_path, self.audit_path.parent):
            with self.subTest(symlink=str(path)):
                saved = path.with_name(path.name + '-retained')
                path.rename(saved)
                path.symlink_to(saved)
                try:
                    with self.assertRaisesRegex(RuntimeError, 'regular owned files'):
                        self.exercise()
                finally:
                    path.unlink()
                    saved.rename(path)

    def test_public_update_changes_only_target_work(self):
        self.continue_fixture()
        self.exercise()
        self.exercise()
        self.assertEqual(self.calls.count('public-resume'), 1)
        self.assertEqual(json.loads((self.output / 'released-upgrade.json').read_text())['result'], 'pass')
        self.args.verify_only = False
        with self.assertRaisesRegex(RuntimeError, 'already continued'):
            self.exercise()
        self.assertEqual(self.calls.count('public-resume'), 1)

    def test_final_tester_requires_current_successful_canonical_results(self):
        self.continue_fixture()
        original = copy.deepcopy(self.after)
        for change in ('missing', 'failed', 'stale', 'context', 'error', 'command', 'empty',
                       'registration', 'later-tester', 'recheck', 'ancestry', 'learning', 'not-ready'):
            with self.subTest(change=change):
                self.after = copy.deepcopy(original)
                result = copy.deepcopy(self.results)
                tester = self.after['attempts'][-1]['tasks'][-1]
                if change == 'missing': self.results_path.unlink()
                if change == 'failed': result['summary']['fail'] = 1
                if change == 'stale': result['candidate_commit'] = self.initial
                if change == 'context': tester['review_context']['candidate_commit'] = self.initial
                if change == 'error': result['error'] = {'kind':'failed'}
                if change == 'command': result['commands'][0]['exit_code'] = 1
                if change == 'empty': result['commands'] = []
                if change == 'registration': self.after['attempts'][-1]['artifacts'] = []
                if change == 'later-tester': self.after['attempts'][-1]['tasks'].append(dict(tester, id='later'))
                if change == 'recheck': tester['role'] = 'failed-command-recheck'
                if change == 'ancestry': self.after['merge_candidates'][0]['candidate_commit'] = self.initial
                if change == 'learning': self.after['attempts'][-1]['learning']['status'] = 'failed'
                if change == 'not-ready': self.after['merge_candidates'][0]['ready_at'] = None
                self.save(self.item_path, self.after)
                if change != 'missing':
                    self.save(self.results_path, result)
                    self.select_proofs()
                with self.assertRaises((RuntimeError, OSError, subprocess.CalledProcessError)):
                    self.exercise()
                self.save(self.results_path, self.results)
        self.assertEqual(self.calls.count('public-resume'), 1)

    def test_public_update_preserves_foreign_work_prior_attempt_and_history(self):
        self.continue_fixture()
        for change in ('foreign', 'prior-attempt', 'completed-task', 'old-artifact', 'old-command'):
            with self.subTest(change=change):
                if change == 'foreign': self.foreign.write_text('{}')
                if change == 'old-artifact': self.old_artifact.write_text('changed')
                if change == 'old-command': self.old_log.write_text('changed')
                item = copy.deepcopy(self.after)
                if change == 'prior-attempt': item['attempts'][0]['status'] = 'complete'
                if change == 'completed-task': item['attempts'][-1]['tasks'][0]['output']['commit'] = self.tip
                self.save(self.item_path, item)
                with self.assertRaisesRegex(RuntimeError, 'retained .* changed|retained first Writer commit'):
                    self.exercise()
                self.save(self.item_path, self.after)
                self.save(self.foreign, {'id':'other'})
                self.save(self.old_artifact, 'retained initial execution\n')
                self.save(self.old_log, 'retained command output\n')
        self.assertEqual(self.calls.count('public-resume'), 1)

    def test_verify_only_rejects_live_fixture_owners(self):
        self.continue_fixture()
        lock = self.source / '.fluent/work/locks' / self.work / (self.writer['id'] + '.lock')
        lock.parent.mkdir(parents=True)
        with lock.open('w') as stream:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(RuntimeError, 'ownership'):
                self.exercise()
        self.assertEqual(self.calls.count('public-resume'), 1)

    def test_verify_only_rejects_missing_or_mismatched_package_evidence(self):
        self.continue_fixture()
        original = copy.deepcopy(self.selection)
        for change in ('missing-selection', 'writer-task', 'tester-task', 'writer-candidate',
                       'writer-command', 'writer-failure', 'writer-hash', 'writer-path',
                       'tester-hash', 'tester-path', 'tester-log-hash', 'missing-log', 'missing-npm', 'continuation-hash'):
            with self.subTest(change=change):
                selection = copy.deepcopy(original)
                writer = selection['writer']
                if change == 'missing-selection': self.args.package_proofs = None
                if change == 'writer-task': writer['task_id'] = 'write-1'
                if change == 'tester-task': selection['tester']['task_id'] = 'write-2'
                if change == 'writer-candidate': writer['candidate'] = self.initial
                if change == 'writer-command': writer['commands'][0]['command'] = 'npm install'
                if change == 'writer-failure': writer['commands'][0]['exit_code'] = 1
                if change == 'writer-hash': writer['commands'][0]['artifacts'][0]['sha256'] = '0' * 64
                if change == 'writer-path': writer['commands'][0]['artifacts'][0]['path'] = str(self.old_artifact.relative_to(self.source))
                if change == 'tester-hash': selection['tester']['results']['sha256'] = '0' * 64
                if change == 'tester-log-hash': selection['tester']['artifacts'][0]['sha256'] = '0' * 64
                if change == 'tester-path': selection['tester']['results']['path'] = str(self.writer_log.relative_to(self.source))
                log = self.results_path.parent / 'command-0.stdout.log'
                if change == 'missing-log': log.unlink()
                npm = self.root / 'npm'
                if change == 'missing-npm': npm.unlink()
                baseline = self.args.continuation_output / 'fixture-before.json'
                original_baseline = baseline.read_bytes()
                if change == 'continuation-hash': baseline.write_text('{}')
                self.save(self.selection_path, selection)
                with self.assertRaises((RuntimeError, OSError)):
                    self.exercise()
                self.args.package_proofs = self.selection_path
                self.save(log, 'original npm package output\n')
                if not npm.exists(): npm.symlink_to(self.root / 'npm-cli.js')
                baseline.write_bytes(original_baseline)
        self.assertEqual(self.calls.count('public-resume'), 1)



class OfflinePackage(unittest.TestCase):
    def test_offline_local_dependency_installs_and_runs(self):
        npm = shutil.which('npm')
        if not npm or not shutil.which('node'):
            self.skipTest('local npm and Node are unavailable; host acceptance remains required')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dependency = root / 'local-dependency'
            dependency.mkdir()
            (dependency / 'package.json').write_text(json.dumps(dict(name='upgrade-local', version='1.0.0', main='index.js')))
            (dependency / 'index.js').write_text('module.exports = "installed-local-dependency";\n')
            (root / 'package.json').write_text(json.dumps(dict(name='upgrade-proof', version='1.0.0',
                dependencies={'upgrade-local': 'file:local-dependency'},
                scripts={'test': 'node test.cjs'})))
            (root / 'test.cjs').write_text("require('assert').strictEqual(require('upgrade-local'), 'installed-local-dependency');\n")
            # Give npm empty config files and a private cache; use its real
            # local-file installation semantics without an operator config read.
            (root / 'npmrc').write_text('')
            environment = dict(os.environ, npm_config_cache=str(root / 'cache'),
                               npm_config_userconfig=str(root / 'npmrc'), npm_config_globalconfig=str(root / 'global-npmrc'))
            (root / 'global-npmrc').write_text('')
            for command in (['install', '--package-lock-only', '--offline', '--ignore-scripts', '--no-audit', '--no-fund'],
                            ['ci', '--offline', '--ignore-scripts', '--no-audit', '--no-fund'], ['test']):
                result = subprocess.run([npm, *command], cwd=root, env=environment,
                                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=30)
                print(result.stdout)
                self.assertEqual(result.returncode, 0, result.stdout)
            self.assertTrue((root / 'node_modules/upgrade-local/package.json').is_file())


class ReleasedHistory(unittest.TestCase):
    """Check the owner's complete export without launching its copied Work."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.source = Path(self.temp.name).resolve()
        fixture = Path(__file__).with_name('released-stranded-history.json').read_bytes()
        # Keep the original owner export intact, including its per-file manifest.
        self.assertEqual(hashlib.sha256(fixture).hexdigest(),
                         '21198c88e16614ecdcbabd8fd56d9f7b94724e39a0f35ff42b51a60bb6f0bc72')
        self.export = json.loads(fixture)
        self.assertEqual(self.export['schema_version'], 1)
        public = self.decode(self.export['public_state'])
        self.before = json.loads(public)
        self.work, self.attempt_id = self.before['id'], 'attempt-2'
        self.initial = 'aa1849f4a3fc99ad4ea651c8e0cdd8470a22c3ad'
        self.base = '885b7df4e1339191621eddb8bedfb0c5f7bca2b5'
        self.git('init', '-q')
        self.git('index-pack', '--stdin', '--strict', input=self.decode(self.export['git_pack']))
        self.assertEqual(set(self.export['git_pack']['commits']), {self.initial, self.base})
        for commit in self.export['git_pack']['commits']:
            self.assertEqual(self.git('cat-file', '-t', commit), 'commit')
            self.assertEqual(set(self.git('ls-tree', '-r', '--name-only', commit).splitlines()),
                             set(self.export['git_pack']['tracked_path_allowlist']))
        self.git('update-ref', 'refs/heads/main', self.base)
        self.git('symbolic-ref', 'HEAD', 'refs/heads/main')
        for relative, record in self.export['files'].items():
            path = Path(relative)
            self.assertFalse(path.is_absolute())
            self.assertFalse(set(path.parts) & {'..', '.git', 'claude-home', 'codex-home',
                                              'pi-home', '.ssh', '.aws', '.env'})
            target = self.source / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(self.decode(record))
        # The public projection differs from the retained 0.6.0 storage schema.
        self.assertEqual(json.loads((self.source / f'.fluent/work/items/{self.work}.json').read_text())['id'],
                         self.work)
        self.assertFalse((self.source / self.initial).exists())
        self.original_files = {p: upgrade.digest(self.source / p) for p in self.export['files']}

    def decode(self, record):
        data = (gzip.decompress(base64.b64decode(record['gzip_base64']))
                if 'gzip_base64' in record else base64.b64decode(record['base64']))
        self.assertEqual(len(data), record['bytes'])
        self.assertEqual(hashlib.sha256(data).hexdigest(), record['sha256'])
        return data

    def git(self, *args, input=None):
        return subprocess.run(['git', *args], cwd=self.source, input=input,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              check=True).stdout.decode().strip()

    def capture(self, item=None):
        return upgrade.retained_history(self.source, item or self.before, self.work, self.attempt_id)

    def test_exact_export_preserves_original_commit_and_all_history(self):
        history = self.capture()
        self.assertEqual(history, self.export['history'])
        self.assertNotIn(self.initial, history['files'])
        self.assertIn(dict(attempt_id=self.attempt_id, artifact=dict(
            producer_id='attempt-2-write-1', path=self.initial)), history['registered_artifacts'])
        # Simulate appended public metadata only; never run this copied Attempt.
        after = copy.deepcopy(self.before)
        after['attempts'][-1]['status'] = 'complete'
        after['attempts'][-1]['artifacts'].append(dict(
            producer_id='later-proof', path='.fluent/work/artifacts/story-upgrade/attempt-2/later.json'))
        upgrade.verify_retained_history(self.source, after, json.loads(json.dumps(history)))
        self.assertEqual({p: upgrade.digest(self.source / p) for p in self.export['files']},
                         self.original_files)
        self.assertEqual(self.git('rev-parse', 'HEAD'), self.base)

    def test_exact_export_rejects_wrong_producer_and_different_output(self):
        for change in ('producer', 'output', 'status', 'base'):
            with self.subTest(change=change):
                before = copy.deepcopy(self.before)
                attempt = before['attempts'][-1]
                writer = next(t for t in attempt['tasks'] if t['id'] == 'attempt-2-write-1')
                artifact = next(a for a in attempt['artifacts'] if a['path'] == self.initial)
                if change == 'producer': artifact['producer_id'] = 'attempt-2-review-tests'
                if change == 'output': writer['output']['commit'] = self.base
                if change == 'status': writer['status'] = 'failed'
                if change == 'base': writer['output']['base_commit'] = 'f' * 40
                with self.assertRaisesRegex(RuntimeError, 'registered Writer commit'):
                    self.capture(before)

    def test_exact_export_rejects_missing_original_object(self):
        history = self.capture()
        for path in (self.source / '.git/objects/pack').glob('*.pack'):
            path.unlink()
        for check in (self.capture, lambda: upgrade.verify_retained_history(self.source, self.before, history)):
            with self.assertRaisesRegex(RuntimeError, 'registered Writer commit object'):
                check()

    def test_exact_export_rejects_changed_registered_identity(self):
        history = self.capture()
        for change in ('producer', 'path', 'removed', 'duplicate'):
            with self.subTest(change=change):
                after = copy.deepcopy(self.before)
                artifacts = after['attempts'][-1]['artifacts']
                artifact = next(a for a in artifacts if a['path'] == self.initial)
                if change == 'producer': artifact['producer_id'] = 'attempt-2-write-2'
                if change == 'path': artifact['path'] = self.base
                if change == 'removed': artifacts.remove(artifact)
                if change == 'duplicate': artifacts.append(copy.deepcopy(artifact))
                with self.assertRaisesRegex(RuntimeError, 'retained registered artifact changed'):
                    upgrade.verify_retained_history(self.source, after, history)

    def test_exact_export_rejects_changed_file_evidence_and_links(self):
        history = self.capture()
        relative = '.fluent/work/artifacts/story-upgrade/attempt-2/attempt-2-review-tests/review.md'
        path = self.source / relative
        original = path.read_bytes()
        path.write_bytes(original + b'changed')
        with self.assertRaisesRegex(RuntimeError, 'retained artifact changed'):
            upgrade.verify_retained_history(self.source, self.before, history)
        path.unlink()
        path.symlink_to(self.source / '.fluent/config.yaml')
        with self.assertRaisesRegex(RuntimeError, 'snapshot needs regular owned files'):
            self.capture()
        with self.assertRaisesRegex(RuntimeError, 'snapshot needs regular owned files'):
            upgrade.verify_retained_history(self.source, self.before, history)


class ReleasedAdmission(IntegratedUpgrade):
    mode = 'released'


if __name__ == '__main__':
    unittest.main()
