"""Run the separately authorized integration checks and retain their evidence.

This driver never attaches evidence, edits Work records directly, or grants acceptance.
The coordinator must attach its receipt and obtain targeted Tests Review.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import shutil
import shlex
import base64
import lzma
import uuid
import re

HOST_CHECKS = {
    'prerequisites': ('plan:step-3', 'bash tests/host/check-integrated-upgrade-prerequisites.sh'),
    'released': ('plan:step-4', 'bash tests/host/check-integrated-released-060-upgrade.sh'),
}

CHECKS = (
    'claude-writer-session-upgrade', 'claude-real-legacy-upgrade',
    'node-worker-upgrade', 'worker-input-upgrade', 'completion-repair-admission',
    'semantic-completion-recovery', 'native-acceptance-prerequisites',
    'native-codex-display-backpressure', 'ssh-fullscreen-backpressure', 'dashboard-terminal',
)
INTAKE_SHA256 = '56085dedebace6547a1f42bc4534caabd23049523225939cc279cad831187b46'
RELEASE_SHA256 = '23b722148602782b4f9092b61df80337b8356da1483b3b810517333499e7ab23'
INITIAL_COMMIT = 'aa1849f4a3fc99ad4ea651c8e0cdd8470a22c3ad'
MISSING_SESSION = '0a06115a-5737-4470-8ec3-f3fab351edd2'
HISTORICAL_CANDIDATE = '6167cf4d134b0290353777240a24940fd78721a7'
HISTORICAL_ENVELOPE_SHA256 = 'd394e05a73df43e99f37528af419bac5b2a7993e3cb84ff349f549db611f912c'
BOUNDARIES = ('corrective_writer_reads_bound_host_failure_without_foreign_access',
              'integrated_completion_retry_preserves_attempt_home_and_gates',
              'targeted_host_review_preserves_pending_writer_finding',
              'confined_pinned_historical_reader_build_preserves_identity')
READ_DIAGNOSTICS = None


def require(condition, reason):
    if not condition:
        raise RuntimeError(reason)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + '\n')


def read_command(args, cwd):
    if READ_DIAGNOSTICS is not None:
        return traced_read_command(args, cwd, READ_DIAGNOSTICS)
    return subprocess.check_output([str(a) for a in args], cwd=cwd, text=True,
                                   env=dict(os.environ, FLUENT_NO_UPDATE_CHECK='1',
                                            FLUENT_QUIET='1'), timeout=30).strip()


def traced_read_command(args, cwd, output):
    """Retain public-read failures without retrying or extending their deadline."""
    area = Path(output) / uuid.uuid4().hex
    area.mkdir(parents=True, mode=0o700)
    trace = area / 'git-trace2.jsonl'
    trace.touch()
    argv = [str(a) for a in args]
    record = dict(argv=argv, cwd=str(cwd), started_at=time.time(),
                  deadline_seconds=30, status=None, timed_out=False,
                  group_settled=False, output_complete=False,
                  executable=shutil.which(argv[0]), git_executable=shutil.which('git'))
    write_json(area / 'read.json', record)
    environment = dict(os.environ, FLUENT_NO_UPDATE_CHECK='1', FLUENT_QUIET='1',
                       GIT_TRACE2_EVENT=str(trace.resolve()))
    child = None
    started = time.monotonic()
    try:
        # Files retain partial bytes on timeout and cannot leave communicate()
        # waiting on a pipe held open by a delayed Git subprocess. Trace2 covers
        # time inside Git; gaps before its start events are not Git runtime.
        with (area / 'stdout.log').open('xb') as stdout, (area / 'stderr.log').open('xb') as stderr:
            child = subprocess.Popen(argv, cwd=cwd, env=environment, stdout=stdout,
                                     stderr=stderr, start_new_session=True)
            record['pid'] = child.pid
            try:
                child.wait(timeout=30)
            except subprocess.TimeoutExpired:
                record['timed_out'] = True
                raise
            finally:
                # These reads own only their subprocess group, never a Fluent
                # Attempt. Use the same unreaped-child authority as Commands.
                record['group_settled'] = settle_coordinator(child)
        require(record['group_settled'], f'public read left a live group; inspect {area}')
        if child.returncode:
            raise subprocess.CalledProcessError(child.returncode, argv)
        return (area / 'stdout.log').read_text().strip()
    except BaseException as error:
        record['error'] = f'{type(error).__name__}: {error}'
        raise
    finally:
        record['status'] = child.returncode if child is not None else None
        record['finished_at'] = time.time()
        record['elapsed_seconds'] = time.monotonic() - started
        record['output_complete'] = record['group_settled']
        # An unsettled descendant can still append. Do not bind changing bytes
        # as complete evidence, or turn a timeout into passing command evidence.
        record['sha256'] = ({name: digest(area / name) for name in
                            ('stdout.log', 'stderr.log', 'git-trace2.jsonl')}
                           if record['output_complete'] else {})
        write_json(area / 'read.json', record)


def clean_candidate(root, candidate=None):
    head = read_command(['git', 'rev-parse', 'HEAD'], root)
    require(candidate is None or head == candidate, 'candidate changed during host checks')
    require(not read_command(['git', 'status', '--porcelain'], root), 'candidate must be clean')
    return head


def attempt_from(item, work, attempt_id):
    require(item.get('id') == work, 'wrong Work Item')
    require(not any(task.get('status') == 'executing' for a in item.get('attempts', [])
                    for task in a.get('tasks', [])), 'a Task is executing; wait for its owner to settle')
    matches = [a for a in item.get('attempts', []) if a.get('id') == attempt_id]
    require(len(matches) == 1, 'exact Attempt is missing')
    return matches[0]


def require_paused(item, work, attempt_id, candidate, source=None, mode='prerequisites'):
    attempt = attempt_from(item, work, attempt_id)
    require(attempt.get('status') == 'needs-user', 'host proof requires a settled pause')
    commits = [t['output']['commit'] for t in attempt.get('tasks', [])
               if isinstance(t.get('output'), dict) and t['output'].get('commit')]
    require(commits and commits[-1] == candidate, 'paused Attempt does not own this exact candidate')
    if attempt.get('pause_kind') != 'writer-completion':
        require(source is not None and attempt.get('pause_kind') == 'uncertain',
                'host proof requires Writer completion or a bound evidence retry')
        require_evidence_retry(Path(source), attempt, work, candidate, mode)
    if source is not None:
        require_released_ownership(item, work, source)
    return attempt


def require_released_ownership(item, work, source):
    for current in item['attempts']:
        owner = OwnedAttempt(['fluent', 'attempt', 'run', work, current['id']], source, {})
        snapshot = owner.snapshot(item)
        require(owner.settled(snapshot), 'host proof requires released execution and setup ownership')
        require(all(e['record'].get('state') in ('settled', 'canceled') and
                    e['record'].get('settled_at') for e in snapshot['executions'].values()),
                'host proof requires durable execution settlement')


def require_evidence_retry(source, attempt, work, candidate, mode):
    """Admit only the completed, candidate-bound Plan check's public retry."""
    area = Path('.fluent/work/artifacts') / work / attempt['id']
    row_id, command = HOST_CHECKS[mode]

    def read(relative, expected=None):
        require(Path(relative).is_relative_to(area), 'retry artifact belongs to another Attempt')
        path = selected_path(source, relative)
        contents = path.read_bytes()
        require(expected is None or 'sha256:' + hashlib.sha256(contents).hexdigest() == expected,
                'retry artifact digest changed')
        return contents.decode('utf-8')

    def review_path(task):
        return str(Path(task['artifact_area']['path']) / 'review.md')

    tasks = attempt['tasks']
    latest = tasks[-1]
    pause = attempt.get('pause_record') or {}
    operation = pause.get('next-operation') or {}
    require(attempt.get('review_state') == 'failed' and latest.get('kind') == 'review' and
            latest.get('role') == 'tests' and latest.get('status') == 'complete',
            'evidence retry requires the latest completed Tests Review')
    require(pause.get('candidate-commit') == candidate and pause.get('pause-kind') == 'uncertain' and
            operation.get('kind') == 'evidence-attach' and operation.get('arguments') == [
                'attempt', 'evidence', 'attach', work, attempt['id'], '--candidate', candidate,
                '--evidence-file', '<path>', '--review-artifact', review_path(latest)],
            'public next operation does not request this exact evidence retry')
    writers = [t for t in tasks if t.get('kind') == 'write']
    require(writers and writers[-1].get('status') == 'complete' and
            (writers[-1].get('output') or {}).get('commit') == candidate,
            'evidence retry requires the completed candidate Writer')
    writer = writers[-1]
    seen = set()
    current = latest
    # A subsequent evidence-only retry may refer to the previous Review. Follow
    # that immutable chain back to the original Plan binding, never another row.
    while True:
        require(current['id'] not in seen, 'cyclic evidence review binding')
        seen.add(current['id'])
        context = current.get('evidence_review_context') or {}
        matches = [r for r in attempt.get('evidence_recoveries', [])
                   if r['id'] == context.get('recovery_id')]
        require(len(matches) == 1, 'evidence recovery is missing or ambiguous')
        recovery = matches[0]
        require(recovery.get('state') == 'needs-evidence' and
                recovery.get('candidate_commit') == candidate and not recovery.get('code_change_classification'),
                'evidence recovery is stale or requests a source correction')
        if current is latest:
            require(recovery == attempt['evidence_recoveries'][-1], 'a later recovery supersedes this Review')
        targets = recovery.get('targets', [])
        require(len(targets) == 1 and targets[0].get('role') == 'tests' and
                targets[0].get('review_task_id') == current['id'], 'evidence target changed')
        target, attachment = targets[0], recovery['attachment']
        require(context == dict(recovery_id=recovery['id'], candidate_commit=candidate,
                                attachment=attachment, prior_review_artifact=target['prior_review_artifact']),
                'Review no longer matches its evidence binding')
        require(target.get('review_digest'), 'evidence retry requires the saved Review digest')
        review = read(review_path(current), target['review_digest'])
        for marker, expected in [('Verdict', 'fail'), ('Disposition', 'evidence-needed'),
                                 ('External-Condition', None), ('Additional-Proof', None)]:
            values = [line.split(':', 1)[1].strip() for line in review.splitlines()
                      if line.startswith(marker + ':')]
            require(len(values) == 1 and values[0] and (expected is None or values[0] == expected),
                    'Review does not request an actionable evidence retry')
        document = json.loads(read(attachment['snapshot_path'], attachment['digest']))
        require(document.get('schema_version') == 1 and document.get('result') == 'fail' and
                document.get('check') == command and document.get('working_directory') == '.',
                'evidence does not describe this failed host command')
        prior = target['prior_review_artifact']
        if prior.endswith('/binding.json'):
            binding = json.loads(read(prior))
            require(all(binding.get(k) == v for k, v in {
                'schema-version': 1, 'work-item-id': work, 'attempt-id': attempt['id'],
                'writer-task-id': writer['id'], 'candidate-commit': candidate, 'plan-row': row_id,
                'command': command, 'working-directory': '.', 'result': 'fail',
                'evidence': dict(path=attachment['snapshot_path'], digest=attachment['digest'])}.items()),
                'Plan evidence binding changed')
            matrix_ref = binding['source-matrix']
            require(matrix_ref['path'] == str(Path(writer['artifact_area']['path']) / 'writer-completion.json'),
                    'Plan evidence belongs to another Writer matrix')
            matrix = json.loads(read(matrix_ref['path'], matrix_ref['digest']))
            rows = [r for r in matrix['rows'] if r['id'] == row_id]
            require(matrix.get('task-id') == writer['id'] and len(rows) == 1 and
                    rows[0].get('responsibility') == 'host' and rows[0].get('state') == 'pending' and
                    rows[0].get('command') == {'command': command, 'working-directory': '.',
                                               'source-row': row_id},
                    'source matrix no longer binds the required host command')
            return
        earlier = [t for t in tasks[:tasks.index(current)] if t.get('kind') == 'review' and
                   t.get('role') == 'tests' and t.get('status') == 'complete' and review_path(t) == prior]
        require(len(earlier) == 1, 'prior evidence Review is missing or stale')
        current = earlier[0]


def host_snapshot(binary, args, root, candidate):
    clean_candidate(root, candidate)
    item = show(binary, args.source_root, args.work)
    require_paused(item, args.work, args.attempt, candidate, args.source_root, args.mode)
    return item


def require_complete(receipt, candidate):
    require(receipt.get('candidate') == candidate, 'prerequisite candidate changed')
    checks = receipt.get('checks', [])
    require(len(checks) == len(CHECKS) + 1 and
            {c['name'] for c in checks} == set(CHECKS) | {'integrated-boundaries'},
            'every constituent prerequisite is required')
    require(all(c.get('status') == 0 for c in checks) and receipt.get('settled') is True,
            'prerequisite execution or cleanup failed')


def direct_evidence(output):
    # Constituents retain their own selected logs and native invocation receipts.
    # Never recurse into disposable projects or provider homes to collect evidence.
    return {p.name: digest(p) for p in sorted(output.iterdir())
            if p.is_file() and not p.is_symlink() and p.name != 'sha256.json'}


def selected_path(root, relative):
    root = root.resolve(strict=True)
    path = Path(relative)
    require(not path.is_absolute() and '..' not in path.parts, 'snapshot path escapes its root')
    require(not any(p in ('claude-home', 'codex-home', 'pi-home', '.ssh', '.aws', '.env')
                    for p in path.parts), 'private state is not evidence')
    result = root / path
    require(result.is_file() and not any(p.is_symlink() for p in [result, *result.parents]
                                       if p.is_relative_to(root)), 'snapshot needs regular owned files')
    require(result.resolve().is_relative_to(root.resolve()), 'snapshot symlink escapes its root')
    return result


def verify_snapshot(root, entries):
    require(isinstance(entries, dict) and len(entries) == 26, 'expected the original 26-file snapshot')
    for relative, expected in entries.items():
        path = selected_path(root, relative)
        require(digest(path) == expected, f'original snapshot changed: {relative}')
    return entries


def lease_held(path):
    # Inspect existing ownership without creating or rewriting execution state.
    try:
        stream = path.open('rb')
    except FileNotFoundError:
        return False
    with stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        fcntl.flock(stream, fcntl.LOCK_UN)
        return False


class OwnedAttempt:
    """Cancel only the explicit foreground command's Work and Attempt."""
    def __init__(self, argv, source, environment):
        self.binary, _, _, self.work, self.attempt = argv[:5]
        self.source, self.environment = Path(source).resolve(strict=True), environment

    def snapshot(self, item=None):
        if item is None:
            item = show(self.binary, self.source, self.work)
        require(item['id'] == self.work, 'cleanup Work identity changed')
        attempts = [a for a in item['attempts'] if a['id'] == self.attempt]
        require(len(attempts) == 1, 'cleanup Attempt identity changed')
        attempt = attempts[0]
        paths = {str(Path(t['artifact_area']['path']) / 'execution.json')
                 for t in attempt['tasks'] if t.get('artifact_area')}
        registered = {a['path'] for a in attempt.get('artifacts', [])
                      if a['producer_id'] == 'local-execution'}
        paths.update(registered)
        executions = {}
        for relative in sorted(paths):
            path = self.source / relative
            if not path.exists():
                require(relative not in registered, 'registered execution record is missing')
                continue
            path = selected_path(self.source, relative)
            value = json.loads(path.read_text())
            owner = value.get('attempt_owner')
            if owner:
                require(Path(owner['project_root']).resolve() == self.source and
                        owner['work_item_id'] == self.work and owner['attempt_id'] == self.attempt,
                        'execution belongs to a different Attempt')
            executions[relative] = dict(record=value,
                                        held=lease_held(path.with_name('execution-owner.lock')))
        locks = self.source / '.fluent/work/locks' / self.work
        setup = {t['id']: lease_held(locks / (t['id'] + '.lock')) for t in attempt['tasks']}
        setup['learner'] = lease_held(locks / (self.attempt + '-learner.lock'))
        return dict(attempt=attempt, executions=executions, setup=setup)

    @staticmethod
    def settled(snapshot):
        return (not any(t.get('status') == 'executing' for t in snapshot['attempt']['tasks']) and
                not any(snapshot['setup'].values()) and
                not any(e['held'] for e in snapshot['executions'].values()))

    def cancel(self, prefix, receipt):
        receipt.update(work=self.work, attempt=self.attempt, source=str(self.source), settled=False)
        # Persist diagnostics even if snapshot validation or public cancellation fails.
        receipt['before'] = self.snapshot()
        argv = [self.binary, 'attempt', 'cancel', self.work, self.attempt]
        receipt['argv'] = argv
        cancel_log = Path(str(prefix) + '-cancel.log')
        with cancel_log.open('xb') as log:
            try:
                result = subprocess.run(argv, cwd=self.source, env=self.environment,
                                        stdout=log, stderr=subprocess.STDOUT, timeout=60)
                receipt['status'] = result.returncode
            finally:
                receipt['log_sha256'] = digest(cancel_log)
        require(result.returncode == 0, 'public Attempt cancellation failed; retain coordinator ownership')
        after = self.snapshot()
        receipt['after'] = after
        stop = after['attempt'].get('cancellation', {})
        require(stop.get('requested_at') and stop.get('settled_at') and not stop.get('resumed_at'),
                'public cancellation has not durably settled')
        require(self.settled(after), 'Attempt execution or setup ownership remains live')
        for relative, old in receipt['before']['executions'].items():
            current = after['executions'].get(relative, {}).get('record', {})
            require(current and current.get('launch_generation') == old['record'].get('launch_generation'),
                    'execution generation changed during cancellation')
        for entry in after['executions'].values():
            record = entry['record']
            require(record['state'] in ('canceled', 'settled') and record.get('settled_at'),
                    'execution has not recorded settlement')
        receipt['settled'] = True


def settle_coordinator(child):
    if child.returncode is None:
        # An unreaped child pins the group identity. Once wait succeeds below,
        # that authority is gone, even if descendants remain in the group.
        try:
            os.killpg(child.pid, signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            # Darwin may deny a signal to an already exited, unreaped child.
            # Only a successful wait AND absent group below prove settlement.
            pass
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=5)
    # Once reaped, a numeric group is only a diagnostic. Never signal it.
    try:
        os.killpg(child.pid, 0)
    except ProcessLookupError:
        return True
    return False


class Commands:
    def __init__(self, root, output, environment):
        self.root, self.output, self.environment = root, output, environment
        self.records = []

    def run(self, name, argv, extra=None, cwd=None, terminal=False, timeout=3600,
            host_check=False):
        prefix = self.output / f'{len(self.records):02}-{name}'
        record = dict(name=name, argv=[str(a) for a in argv], cwd=str(cwd or self.root),
                      started_at=time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), status=None)
        self.records.append(record)
        write_json(prefix.with_suffix('.json'), record)
        environment = dict(self.environment, **(extra or {}))
        actual = record['argv']
        owner = (OwnedAttempt(actual, record['cwd'], environment)
                 if len(actual) >= 5 and actual[1:3] == ['attempt', 'run'] else None)
        if owner:
            require(owner.settled(owner.snapshot()), 'foreground Attempt is already owned by a live runner')
        if terminal:
            # script supplies a controlling PTY and retains complete terminal bytes.
            actual = ['/usr/bin/script', '-q', str(prefix.with_suffix('.pty')), *actual]
        with prefix.with_suffix('.log').open('xb') as log:
            child = subprocess.Popen(actual, cwd=cwd or self.root, env=environment,
                                     stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            record['coordinator_pid'] = child.pid
            try:
                record['status'] = child.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                record['timed_out'] = True
                raise
            finally:
                record['group_settled'] = False
                try:
                    if host_check and record['status'] is None:
                        # Constituent harnesses own their disposable Attempts.
                        # This wrapper has no authority over those identities;
                        # killing the harness would bypass its public cleanup.
                        record['cleanup_pending'] = True
                        raise RuntimeError('host check has not returned; its owner must settle cleanup; '
                                           f'retain coordinator {child.pid} and inspect {prefix}.log')
                    if owner:
                        if record['status'] != 0 or not owner.settled(owner.snapshot()):
                            record['unexpected_live_owner'] = record['status'] == 0
                            record['attempt_cleanup'] = {}
                            owner.cancel(prefix, record['attempt_cleanup'])
                            child.poll()
                        else:
                            record['attempt_settled'] = True
                    # A failed cancellation must not fall through to force-kill
                    # the coordinator before worker/setup owners have settled.
                    record['group_settled'] = settle_coordinator(child)
                except BaseException as error:
                    record['cleanup_error'] = str(error)
                    raise
                finally:
                    log_path = prefix.with_suffix('.log')
                    record['log_complete'] = child.returncode is not None
                    if not record['log_complete']:
                        # The owner may still append output while settling. Keep
                        # an immutable failure snapshot with an honest boundary.
                        snapshot = Path(str(prefix) + '-incomplete.log')
                        shutil.copyfile(log_path, snapshot)
                        record['log_snapshot'] = str(snapshot)
                        log_path = snapshot
                    record['log_sha256'] = digest(log_path)
                    write_json(prefix.with_suffix('.json'), record)
        require(record['status'] == 0 and record['group_settled'] and
                not record.get('unexpected_live_owner'), f'{name} failed; inspect {prefix}.log')
        return prefix.with_suffix('.log')


def production_binary(commands):
    target = commands.output / 'production-build'
    commands.run('build-production', ['cargo', 'build', '--offline', '--no-default-features',
                                      '--bin', 'fluent', '--target-dir', target])
    binary = target / 'debug/fluent'
    version = read_command([binary, 'version'], commands.root)
    candidate = clean_candidate(commands.root)
    require(candidate[:8] in version, 'production executable does not identify the candidate')
    write_json(commands.output / 'binary.json', dict(path=str(binary), sha256=digest(binary),
                                                    version=version, candidate=candidate))
    return binary


def show(binary, source, work):
    return json.loads(read_command([binary, 'work-item', 'show', work], source))


def prerequisites(args, commands, candidate, binary):
    before = host_snapshot(binary, args, commands.root, candidate)
    write_json(commands.output / 'before.json', before)
    check_records = []
    receipt = dict(candidate=candidate, intake_sha256=INTAKE_SHA256, checks=check_records,
                   settled=False, acceptance='pending targeted Tests Review')
    try:
        for name in CHECKS:
            require(host_snapshot(binary, args, commands.root, candidate) == before,
                    'paused Work changed during host checks')
            evidence = commands.output / name
            environment = {
                'FLUENT_CLAUDE_SESSION_HOST': '1', 'FLUENT_CLAUDE_LEGACY_HOST': '1',
                'FLUENT_NODE_WORKER_HOST': '1', 'FLUENT_SSH_BACKPRESSURE_HOST': '1',
                'FLUENT_NATIVE_DISPOSABLE_FIXTURES': '1',
                'FLUENT_CLAUDE_SESSION_OUTPUT_DIR': str(evidence),
                'FLUENT_CLAUDE_LEGACY_OUTPUT_DIR': str(evidence),
                'FLUENT_WORKER_INPUT_OUTPUT_DIR': str(evidence),
                'FLUENT_SEMANTIC_COMPLETION_OUTPUT': str(evidence),
                'FLUENT_NATIVE_ACCEPTANCE_OUTPUT': str(evidence),
                'FLUENT_SSH_BACKPRESSURE_OUTPUT': str(evidence),
                'FLUENT_DASHBOARD_HOST_OUTPUT_DIR': str(evidence),
            }
            if name in ('native-acceptance-prerequisites', 'native-codex-display-backpressure'):
                environment['FLUENT_TEST_REAL_NATIVE_AGENT_HOST'] = '1'
            environment['FLUENT_NATIVE_HOST_OUTPUT_DIR'] = str(evidence)
            if name == 'dashboard-terminal':
                environment['FLUENT_TEST_REAL_TERMINAL_HOST'] = '1'
            try:
                print(f'Running bash tests/host/check-{name}.sh', flush=True)
                commands.run(name, ['bash', f'tests/host/check-{name}.sh'], environment,
                             terminal=name == 'native-acceptance-prerequisites', host_check=True)
            finally:
                check_records.append(commands.records[-1])
                # Older source drivers choose their own output directory. Keep
                # their complete selected logs, not just their console tails.
                log = commands.output / f'{len(commands.records)-1:02}-{name}.log'
                if not evidence.exists() and log.exists():
                    for line in log.read_text(errors='replace').splitlines():
                        if line.startswith(('evidence=', 'output=')):
                            evidence = Path(line.split('=', 1)[1])
                if evidence.is_dir():
                    selected = commands.output / 'selected' / name
                    selected.mkdir(parents=True)
                    for path in evidence.iterdir():
                        if path.is_file() and not path.is_symlink() and path.suffix in ('.log', '.txt', '.jsonl', '.stdout', '.stderr'):
                            shutil.copyfile(path, selected / path.name)
                    # Explicit native proof files contain assigned generation and
                    # cleanup results. Provider homes stay outside the selection.
                    for relative in ('result.json', 'binary.json', 'claude-invocation.json',
                                     'pi-invocation.json', 'claude-observation.json', 'pi-observation.json',
                                     'codex-runtime/display-proof.json', 'codex-runtime/execution-settled.json'):
                        path = evidence / relative
                        if path.is_file() and not path.is_symlink():
                            target = selected / relative.replace('/', '-')
                            shutil.copyfile(path, target)
                    write_json(selected / 'sha256.json', direct_evidence(selected))
                    commands.records[-1]['evidence_directory'] = str(evidence)
        require(host_snapshot(binary, args, commands.root, candidate) == before,
                'paused Work changed before integrated boundary checks')
        environment = dict(FLUENT_INTEGRATED_UPGRADE_REQUIRE_SEATBELT='1',
                           FLUENT_BIN_OVERRIDE=str(binary), FLUENT_SEMANTIC_COMPLETION_HOST='1',
                           FLUENT_SEMANTIC_COMPLETION_OUTPUT=str(commands.output / 'integrated-recovery'))
        (commands.output / 'integrated-recovery').mkdir()
        try:
            commands.run('integrated-boundaries', ['cargo', 'nextest', 'run', '--offline',
                         '--features', 'test-support', '--test', 'binary', '--no-tests', 'fail',
                         '--no-fail-fast', '--test-threads', '1', '--success-output', 'immediate',
                         '-E', ' | '.join(f'test(={name})' for name in BOUNDARIES)], environment,
                         host_check=True)
        finally:
            check_records.append(commands.records[-1])
    finally:
        after = show(binary, args.source_root, args.work)
        write_json(commands.output / 'after.json', after)
        try:
            require(host_snapshot(binary, args, commands.root, candidate) == before and after == before,
                    'paused Work changed during host checks')
            receipt['settled'] = all(c.get('group_settled') for c in check_records)
        finally:
            write_json(commands.output / 'prerequisites.json', receipt)
    require_complete(receipt, candidate)


def require_registered_writer_commit(source, work, attempt, artifact):
    """Validate the producing Writer and its actual Git objects in this repository."""
    tasks = [t for t in attempt['tasks'] if t['id'] == artifact['producer_id']]
    require(len(tasks) == 1, 'registered Writer commit needs one exact producing Task')
    task = tasks[0]
    output = task.get('output') or {}
    commit, base = artifact['path'], output.get('base_commit')
    require(task.get('kind') == 'write' and task.get('status') == 'complete' and
            task.get('work_item_id') == work and task.get('attempt_id') == attempt['id'] and
            output.get('commit') == commit,
            'registered Writer commit does not match its completed producing Task')
    require(all(isinstance(oid, str) and len(oid) in (40, 64) and
                all(c in '0123456789abcdef' for c in oid) for oid in (commit, base)),
            'registered Writer commit requires full commit and base object IDs')
    try:
        for oid in (commit, base):
            require(read_command(['git', '--no-replace-objects', 'cat-file', '-t', oid], source) == 'commit',
                    'registered Writer commit or base is not a commit object')
        # Source HEAD may still precede an unlanded Writer's separate workspace.
        # Its saved base defines ancestry here; the ready-candidate check also
        # requires the original Writer commit to remain in the final candidate.
        read_command(['git', '--no-replace-objects', 'merge-base', '--is-ancestor', base, commit], source)
    except subprocess.CalledProcessError as error:
        raise RuntimeError('registered Writer commit object or base ancestry is missing') from error


def retained_history(source, before, work, attempt_id):
    """Preserve unrelated records and completed history while the target advances."""
    require(Path(work).name == work, 'invalid target Work identity')
    target = selected_path(source, f'.fluent/work/items/{work}.json')
    require(json.loads(target.read_text()).get('id') == work, 'target Work record changed')
    files = {str(p.relative_to(source)): digest(selected_path(source, p.relative_to(source)))
             for p in (source / '.fluent/work/items').rglob('*') if p.is_file() and p != target}
    prior = [a for a in before['attempts'] if a['id'] != attempt_id]
    completed = [t for a in before['attempts'] for t in a['tasks'] if t.get('status') == 'complete']
    settled_tasks = [t for a in before['attempts'] for t in a['tasks']
                     if t.get('status') in ('complete', 'failed', 'canceled')]
    for task in settled_tasks:
        area = Path(task['artifact_area']['path'])
        require(area.is_relative_to(Path('.fluent/work/artifacts') / work), 'foreign retained Task artifacts')
        # Direct Task outputs contain evidence; private provider homes are directories.
        for path in [*(source / area).glob('*'), *(source / area / 'commands').rglob('*')]:
            if path.is_file():
                relative = str(path.relative_to(source))
                files[relative] = digest(selected_path(source, relative))
    registered = []
    for attempt in before['attempts']:
        for artifact in attempt.get('artifacts', []):
            relative = artifact['path']
            area = Path('.fluent/work/artifacts') / work / attempt['id']
            if Path(relative).is_relative_to(area):
                files[relative] = digest(selected_path(source, relative))
            else:
                require_registered_writer_commit(source, work, attempt, artifact)
            require(attempt['artifacts'].count(artifact) == 1, 'registered artifact is ambiguous')
            registered.append(dict(attempt_id=attempt['id'], artifact=dict(artifact)))
    return dict(files=files, prior_attempts=prior, completed_tasks=completed,
                work=work, registered_artifacts=registered)


def verify_retained_history(source, after, history):
    require(after.get('id') == history['work'], 'retained Work identity changed')
    for entry in history['registered_artifacts']:
        attempts = [a for a in after['attempts'] if a['id'] == entry['attempt_id']]
        artifact = entry['artifact']
        require(len(attempts) == 1 and attempts[0].get('artifacts', []).count(artifact) == 1,
                'retained registered artifact changed')
        area = Path('.fluent/work/artifacts') / history['work'] / entry['attempt_id']
        if not Path(artifact['path']).is_relative_to(area):
            require_registered_writer_commit(source, history['work'], attempts[0], artifact)
    for relative, sha in history['files'].items():
        require(digest(selected_path(source, relative)) == sha, f'retained artifact changed: {relative}')
    for prior in history['prior_attempts']:
        require([a for a in after['attempts'] if a['id'] == prior['id']] == [prior],
                'retained Attempt changed')
    tasks = [t for a in after['attempts'] for t in a['tasks']]
    for task in history['completed_tasks']:
        require([t for t in tasks if t['id'] == task['id']] == [task], 'retained completed Task changed')


def package_task_candidate(source, attempt, task, role, output):
    """Bind Writer output and canonical Tester results through their native fields."""
    writers = [t for t in attempt['tasks'] if t.get('kind') == 'write']
    require(writers and writers[-1].get('status') == 'complete', 'missing completed final Writer')
    writer_commit = (writers[-1].get('output') or {}).get('commit')
    require(writer_commit, 'missing Writer candidate')
    if role == 'writer':
        require(task == writers[-1], 'package proof belongs to a stale Writer')
        return writer_commit
    testers = [t for t in attempt['tasks'] if t.get('kind') == 'tester' and t.get('role') == 'tester']
    require(testers and task == testers[-1] and
            (task.get('review_context') or {}).get('candidate_commit') == writer_commit,
            'package proof must bind the final canonical Tester candidate')
    relative = str(Path(task['artifact_area']['path']) / 'tester-results.json')
    require(dict(producer_id=task['id'], path=relative) in attempt.get('artifacts', []),
            'final Tester result is not registered to this Task')
    path = selected_path(source, relative)
    contents = path.read_bytes()
    results = json.loads(contents)
    checks, summary, tests = results.get('commands'), results.get('summary'), results.get('tests')
    require(isinstance(checks, list) and all(isinstance(c, dict) for c in checks) and
            [c.get('command') for c in checks] == list(PACKAGE_COMMANDS) and
            all(c.get('test_harness') == 'shell-harness' for c in checks),
            'Tester must run the unchanged original package commands')
    # The shell extractor does not parse Node's checkmark/info reporter. For
    # these exact commands, numeric exits and bound streams establish success
    # even when the canonical result contains no parsed test entries.
    empty_summary = (tests == [] and summary == {'total': 0, 'pass': 0, 'fail': 0, 'skipped': 0} and
                     all(type(value) is int for value in summary.values()))
    require(results.get('candidate_commit') == writer_commit and results.get('error') is None and
            all(type(c.get('exit_code')) is int and c['exit_code'] == 0 for c in checks) and
            isinstance(summary, dict) and summary.get('fail') == 0 and
            (type(summary.get('pass')) is int and summary['pass'] > 0 or empty_summary) and
            isinstance(tests, list) and all(isinstance(t, dict) and t.get('status') != 'fail' for t in tests),
            'final Tester results are missing, failed or stale')
    (output / 'tester-results.json').write_bytes(contents)
    return writer_commit


PACKAGE_COMMANDS = ('npm ci --offline --ignore-scripts --no-audit --no-fund', 'npm test')


WRITER_RETRIES = {
    'claude-session-retry': (3, 'missing-claude-conversation', 'claude-session-retries'),
    'writer-completion-context-retry': (1, 'repaired-context', 'writer-completion-context-retries'),
    'writer-completion-no-change-retry': (2, 'invalid-completion', 'writer-completion-no-change-retries'),
}


def require_package_artifact_assignment(source, attempt, task):
    """Accept only the original Task area or its audited public retry assignment."""
    work, attempt_id, task_id = attempt['work_item_id'], attempt['id'], task['id']
    require(all(value and value not in ('.', '..') and Path(value).name == value
                for value in (work, attempt_id, task_id)), 'invalid package Task identity')
    root = Path('.fluent/work/artifacts') / work / attempt_id
    original_area = dict(path=str(root / task_id))
    approvals = [op for op in attempt.get('recovery_operations', [])
                 if op.get('operation-kind') in WRITER_RETRIES and op.get('task-id') == task_id]
    inputs = task.get('input_artifacts', [])
    if task.get('artifact_area') == original_area and not approvals and not any(
            ref.get('producer_id') in WRITER_RETRIES for ref in inputs):
        return
    require(task.get('kind') == 'write' and len(approvals) == 1,
            'package Task artifact identity changed without one approved Writer retry')
    operation = approvals[0]
    kind = operation['operation-kind']
    version, retry_source, directory = WRITER_RETRIES[kind]
    reference = dict(producer_id=kind, path=str(root / directory / task_id / 'recovery.json'))
    require(inputs.count(reference) == 1 and attempt.get('artifacts', []).count(reference) == 1,
            'package retry lost its assigned and registered recovery audit')
    audit_bytes = selected_path(source, reference['path']).read_bytes()
    audit = json.loads(audit_bytes)
    require(audit_bytes == json.dumps(audit, ensure_ascii=False, indent=2).encode(),
            'package recovery audit bytes changed')
    before = audit.get('before', {})
    require(audit.get('schema_version') == version and
            audit.get('source', 'repaired-context') == retry_source and audit.get('task_id') == task_id and
            before.get('work_item_id') == work and before.get('id') == attempt_id,
            'package recovery audit identity changed')
    failed = [t for t in before.get('tasks', []) if t.get('id') == task_id]
    require(len(failed) == 1 and failed[0].get('artifact_area') == original_area and
            failed[0].get('status') in ('failed', 'needs-user') and failed[0].get('output') is None,
            'package recovery audit lost the original failed Task')
    failed = failed[0]
    # prepared_task preserves the assignment and adds exactly this audit input.
    # Execution may update only lifecycle fields and the final Writer output.
    mutable = {'status', 'started_at', 'completed_at', 'output', 'artifact_area', 'input_artifacts'}
    require({k: v for k, v in task.items() if k not in mutable} ==
            {k: v for k, v in failed.items() if k not in mutable} and
            inputs == [*failed.get('input_artifacts', []), reference],
            'package recovery Task assignment changed')
    writes = failed.get('workspace_access', {}).get('writes', [])
    require(writes and writes[0].get('path'), 'package retry lost its assigned workspace')
    if retry_source == 'repaired-context':
        candidate = (failed.get('writer_correction_assignment') or {}).get('source_candidate_commit')
    else:
        prior = before['tasks'][:before['tasks'].index(failed)]
        candidate = next(((t.get('output') or {}).get('commit') for t in reversed(prior)
                          if t.get('output')), None)
    require(candidate and isinstance(audit.get('resolution'), str) and audit['resolution'].strip(),
            'package retry lost its candidate or authorization')
    # RecoveryOperationRequest hashes the compact serde JSON audit, then hashes
    # these UTF-8 request fields with a big-endian u64 length before each value.
    audit_digest = hashlib.sha256(json.dumps(audit, ensure_ascii=False, separators=(',', ':')).encode()).hexdigest()
    request = {'operation-kind': kind, 'work-item-id': work, 'attempt-id': attempt_id,
               'candidate-commit': candidate, 'task-id': task_id, 'evidence-digest': None,
               'workspace-path': writes[0]['path'], 'authorization': audit['resolution'],
               'input-digest': 'sha256:' + audit_digest}
    request = {k: v.strip() if v is not None else None for k, v in request.items()}
    identity = hashlib.sha256()
    for value in request.values():
        raw = (value if value is not None else '<none>').encode()
        identity.update(len(raw).to_bytes(8, 'big'))
        identity.update(raw)
    operation_id = 'recovery-' + identity.hexdigest()
    require(operation.get('schema-version') == 1 and
            all(operation.get(k) == v for k, v in request.items()) and
            operation.get('operation-id') == operation_id and
            operation.get('authorization-digest') == 'sha256:' + hashlib.sha256(request['authorization'].encode()).hexdigest() and
            operation.get('result') == ('completion-retry' if retry_source == 'invalid-completion' else 'applied'),
            'package recovery operation or audit changed')
    require(task.get('artifact_area') == dict(path=str(root / 'writer-completion-retry-runs' / operation_id)),
            'package retry artifact assignment changed')


def installed_npm():
    path = Path(shutil.which('npm') or '')
    require(path.is_symlink(), 'installed npm must resolve through its retained symlink')
    target = path.resolve(strict=True)
    require(target.is_file(), 'installed npm runtime is missing')
    return dict(path=str(path), target=str(target), sha256=digest(target))


def selected_task_bytes(source, task, entry):
    area = Path(task['artifact_area']['path'])
    require(Path(entry['path']).is_relative_to(area), 'package evidence is outside its exact Task artifacts')
    contents = selected_path(source, entry['path']).read_bytes()
    require(hashlib.sha256(contents).hexdigest() == entry['sha256'], 'original package evidence changed')
    return contents


def literal_shell_commands(command):
    """Recognize external literal argv joined by && or ;, never infer shell state.

    This deliberately small protocol excludes substitutions, assignments,
    functions, aliases, sourced scripts, redirects and nested shells. Unknown
    syntax makes that Bash request's cwd effect unproven.
    """
    if not isinstance(command, str) or any(c in command for c in '$`\n\r'):
        return None
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        lexer.commenters = ''
        tokens = list(lexer)
    except ValueError:
        return None
    external = {'git', 'npm', 'npx', 'node', 'python3', 'python', 'ls', 'cat',
                'rg', 'grep', 'sed', 'head', 'tail', 'wc', 'find', 'mkdir', 'rm',
                'cp', 'mv', 'chmod', 'diff', 'pwd', 'echo', 'printf', 'true'}
    commands, current = [], []
    for token in tokens:
        if token in ('&&', ';'):
            if not current:
                return None
            commands.append(current)
            current = []
        elif token and all(c in '();<>|&' for c in token):
            return None
        else:
            current.append(token)
    if not current:
        return None
    commands.append(current)
    return commands if all(argv[0] in external for argv in commands) else None


def literal_workspace_reset(command, workspace):
    """Recognize one literal cd back to the assigned workspace."""
    if not isinstance(command, str) or any(c in command for c in '$`\n\r;&|<>()'):
        return False
    try:
        argv = shlex.split(command)
    except ValueError:
        return False
    return (len(argv) == 2 and argv[0] == 'cd' and
            Path(argv[1]).is_absolute() and Path(argv[1]).resolve() == workspace)


def native_foreground_result(request, block, result, returned, *, commit_result=False):
    """Validate actual Claude foreground status without synthesizing an exit."""
    inputs, native = block.get('input', {}), result.get('tool_use_result')
    require(request.get('type') == 'assistant' and result.get('type') == 'user' and
            request.get('parent_tool_use_id') is None and result.get('parent_tool_use_id') is None and
            block.get('name') == 'Bash' and
            set(inputs) <= {'command', 'description', 'timeout', 'run_in_background'} and
            inputs.get('run_in_background', False) is False,
            'request is not standalone foreground Bash')
    fields = {'stdout', 'stderr', 'interrupted', 'isImage', 'noOutputExpected', 'exitCode'}
    if commit_result:
        fields.add('gitOperation')
    require(returned.get('is_error') is False and isinstance(native, dict) and
            set(native) <= fields and
            isinstance(native.get('stdout'), str) and isinstance(native.get('stderr'), str) and
            native.get('interrupted') is False and native.get('isImage') is False and
            native.get('noOutputExpected') is False and
            ('exitCode' not in native or type(native['exitCode']) is int and native['exitCode'] == 0) and
            len(result['message']['content']) == 1,
            'native result failed, is missing or uses an unsupported format')
    return native


def native_package_results(source, task, proof, output):
    """Pair canonical Claude Bash requests with synchronous native results.

    Claude reports foreground Bash success as is_error=false with a structured,
    uninterrupted tool_use_result. It does not emit a numeric exit on success.
    Only exact standalone commands qualify, so that status is the npm status.
    Unknown provider/result shapes cannot establish package execution.
    """
    area = Path(task['artifact_area']['path'])
    bindings = {}
    for key, name in (('execution', 'execution.json'), ('receipt', 'execution-receipt')):
        entry = proof.get(key, {})
        require(entry.get('path') == str(area / name), 'Writer invocation artifact changed')
        bindings[key] = selected_task_bytes(source, task, entry)
        (output / ('writer-' + name)).write_bytes(bindings[key])
    execution = json.loads(bindings['execution'])
    owner = execution.get('attempt_owner', {})
    generation = execution.get('launch_generation')
    pid = execution.get('leader_pid', 0)
    require(execution.get('schema_version') == 2 and execution.get('state') == 'settled' and
            execution.get('settled_at') and isinstance(generation, str) and len(generation) == 32 and
            all(c in '0123456789abcdef' for c in generation) and generation == proof.get('generation') and
            pid > 0 and execution.get('process_group_id') == pid and
            bindings['receipt'] == f'2 {generation} {pid:010d}\n'.encode() and
            Path(owner.get('project_root', '')).resolve() == source.resolve() and
            owner.get('work_item_id') == task['work_item_id'] and owner.get('attempt_id') == task['attempt_id'],
            'Writer invocation generation or ownership changed')
    checks = proof.get('commands', [])
    require([c.get('command') for c in checks] == list(PACKAGE_COMMANDS) and
            all(len(c.get('artifacts', [])) == 1 for c in checks),
            'Writer must prove the original successful package commands')
    raw = None
    for index, check in enumerate(checks):
        entry = check['artifacts'][0]
        require(Path(entry['path']).is_relative_to(area), 'package evidence is outside its exact Task artifacts')
        current = selected_task_bytes(source, task, entry)
        require(entry['path'] == str(area / 'transcript.jsonl') and (raw is None or current == raw),
                'Writer package evidence must use the current canonical transcript')
        raw = current
        (output / f'writer-package-{index}-0.log').write_bytes(raw)
    rows = [json.loads(line) for line in raw.decode('utf-8').splitlines() if line.strip()]
    require(rows and all(isinstance(row, dict) for row in rows), 'unsupported native Writer evidence')
    session = proof.get('session_id')
    starts = [i for i, row in enumerate(rows)
              if row.get('type') == 'system' and row.get('subtype') == 'init']
    require(len(starts) == 1, 'canonical transcript must contain one native initialization')
    start = starts[0]
    # Claude can invalidate its UI before initialization. Accept only this
    # notification, with no payload that could hide tool effects or a new cwd.
    for row in rows[:start]:
        require(set(row) == {'type', 'subtype', 'event', 'uuid', 'session_id'} and
                row['type'] == 'system' and row['subtype'] == 'ui_invalidate' and
                row['event'] == 'ui.render' and row['session_id'] == session and
                isinstance(row['uuid'], str) and re.fullmatch(
                    r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', row['uuid']),
                'unsupported notification before native initialization')
    init = rows[start]
    writes = task.get('workspace_access', {}).get('writes', [])
    require(session and init.get('type') == 'system' and init.get('subtype') == 'init' and
            init.get('session_id') == session and len(writes) == 1 and
            Path(init.get('cwd', '')).is_absolute() and
            Path(init['cwd']).resolve() == (source / writes[0]['path']).resolve(),
            'Writer invocation session or assigned workspace changed')
    requests, results, positions = {}, {}, {}
    workspace = Path(init['cwd']).resolve()
    cwd_safe, pending_reset = True, None
    for position, row in enumerate(rows[start + 1:], start + 1):
        require(row.get('session_id') == session, 'foreign native invocation result')
        require(not (row.get('type') == 'system' and row.get('subtype') == 'init'),
                'multiple native invocations in canonical transcript')
        for block in row.get('message', {}).get('content', []):
            if not isinstance(block, dict):
                continue
            if block.get('type') == 'tool_use':
                tool_id = block.get('id')
                require(tool_id and tool_id not in requests, 'duplicate native tool request')
                inputs = block.get('input', {})
                pending_reset = None
                safe = (block.get('name') == 'Bash' and cwd_safe and
                        literal_shell_commands(inputs.get('command')) is not None)
                if block.get('name') == 'Bash' and literal_shell_commands(inputs.get('command')) is None:
                    cwd_safe = False
                    if literal_workspace_reset(inputs.get('command'), workspace):
                        pending_reset = tool_id
                requests[tool_id] = (row, block, safe)
                positions[('request', tool_id)] = position
            elif block.get('type') == 'tool_result':
                tool_id = block.get('tool_use_id')
                require(tool_id in requests and tool_id not in results, 'missing or duplicate native tool result identity')
                results[tool_id] = (row, block)
                positions[('result', tool_id)] = position
                if tool_id == pending_reset:
                    request, requested, _ = requests[tool_id]
                    try:
                        native_foreground_result(request, requested, row, block)
                    except (RuntimeError, KeyError, TypeError):
                        pass
                    else:
                        cwd_safe = True
                    pending_reset = None
    prior_result = -1
    for check in checks:
        tool_id = check.get('tool_use_id')
        require(tool_id in requests and tool_id in results, 'missing native package request or result')
        require(positions[('request', tool_id)] > prior_result,
                'native package commands must run sequentially')
        prior_result = positions[('result', tool_id)]
        require(prior_result > positions[('request', tool_id)], 'native result must follow its request')
        request, block, safe = requests[tool_id]
        result, returned = results[tool_id]
        inputs = block.get('input', {})
        require(inputs.get('command') == check['command'] and safe,
                'package request is not standalone Bash in the assigned workspace')
        native = native_foreground_result(request, block, result, returned)
        require('exit_code' not in check or 'exitCode' in native and
                type(check['exit_code']) is int and check['exit_code'] == native['exitCode'],
                'selected numeric exit is absent from the native result')
    commits = []
    for tool_id, (request, block, safe) in requests.items():
        argv = literal_shell_commands(block.get('input', {}).get('command'))
        if not argv or not any(a[:2] == ['git', 'commit'] for a in argv):
            continue
        # Only add commands may precede one final commit; no foreign repository
        # options, shell wrappers or post-commit commands can provide its status.
        require(safe and argv[-1][:2] == ['git', 'commit'] and
                all(a[:2] == ['git', 'add'] for a in argv[:-1]) and tool_id in results,
                'producing commit request is not bound to the assigned workspace')
        result, returned = results[tool_id]
        native = native_foreground_result(request, block, result, returned, commit_result=True)
        headers = re.findall(r'^\[([^\]\r\n]+) ([0-9a-f]{7,40})\] [^\r\n]*$', native['stdout'], re.MULTILINE)
        require(len(headers) == 1, 'missing actual producing commit result')
        branch, sha = headers[0]
        actual = read_command(['git', '--no-replace-objects', 'rev-parse', '--verify', sha + '^{commit}'], source)
        if 'gitOperation' in native:
            operation = native['gitOperation']
            require(isinstance(operation, dict) and set(operation) == {'commit'},
                    'unsupported native Git operation')
            commit = operation['commit']
            require(isinstance(commit, dict) and set(commit) == {'sha', 'kind', 'branch'} and
                    commit['kind'] == 'committed' and commit['branch'] == branch and
                    isinstance(commit['sha'], str) and re.fullmatch(r'[0-9a-f]{7,40}', commit['sha']),
                    'unsupported native commit metadata')
            metadata_commit = read_command(['git', '--no-replace-objects', 'rev-parse', '--verify',
                                           commit['sha'] + '^{commit}'], source)
            require(metadata_commit == actual, 'native commit metadata disagrees with stdout')
        if actual == task.get('output', {}).get('commit'):
            require(positions[('request', tool_id)] > prior_result and
                    positions[('result', tool_id)] > positions[('request', tool_id)],
                    'package successes must precede the actual producing commit')
            commits.append(tool_id)
    require(len(commits) == 1, 'missing or ambiguous actual producing commit evidence')
    return dict(session_id=session, generation=generation, tool_use_ids=[c['tool_use_id'] for c in checks],
                producing_commit=task['output']['commit'], commit_tool_use_id=commits[0])


def verify_packages(args, source, attempt, tip, commands):
    require(args.package_proofs is not None, 'select original package evidence with --package-proofs')
    contents = selected_path(args.package_proofs.parent, args.package_proofs.name).read_bytes()
    selection = json.loads(contents)
    require(selection.get('schema_version') == 1 and selection.get('work') == attempt['work_item_id'] and
            selection.get('attempt') == attempt['id'], 'package selection belongs to another Work or Attempt')
    for role in ('writer', 'tester'):
        proof = selection[role]
        tasks = [t for t in attempt['tasks'] if t['id'] == proof['task_id']]
        require(len(tasks) == 1 and tasks[0].get('status') == 'complete' and
                tasks[0].get('kind') == ('write' if role == 'writer' else 'tester') and
                tasks[0].get('work_item_id') == selection['work'] and tasks[0].get('attempt_id') == attempt['id'],
                f'{role} package evidence must belong to the completed assigned Task')
        task = tasks[0]
        require_package_artifact_assignment(source, attempt, task)
        commit = package_task_candidate(source, attempt, task, role, commands.output)
        require(proof.get('candidate') == commit, f'{role} package candidate changed')
        commands.run(f'{role}-candidate-ancestry', ['git', 'merge-base', '--is-ancestor', commit, tip], cwd=source)
        if role == 'writer':
            write_json(commands.output / 'writer-package-results.json',
                       native_package_results(source, task, proof, commands.output))
        else:
            entry = proof['results']
            expected = str(Path(task['artifact_area']['path']) / 'tester-results.json')
            require(entry['path'] == expected, 'package selection must use the canonical Tester result')
            raw = (commands.output / 'tester-results.json').read_bytes()
            require(hashlib.sha256(raw).hexdigest() == entry['sha256'], 'selected Tester results changed')
            checks = json.loads(raw)['commands']
            logs = []
            for index, check in enumerate(checks):
                for stream in ('stdout_log', 'stderr_log'):
                    relative = Path(check[stream])
                    require(not relative.is_absolute() and '..' not in relative.parts, 'Tester log escapes its Task')
                    path = Path(task['artifact_area']['path']) / relative
                    entries = [e for e in proof.get('artifacts', []) if e['path'] == str(path)]
                    require(len(entries) == 1, 'select each original Tester command log exactly once')
                    raw = selected_task_bytes(source, task, entries[0])
                    name = f'tester-package-{index}-{stream}.log'
                    (commands.output / name).write_bytes(raw)
                    logs.append(dict(path=str(path), sha256=hashlib.sha256(raw).hexdigest(), copy=name))
            write_json(commands.output / 'tester-package-logs.json', logs)
    (commands.output / 'package-selection.json').write_bytes(contents)


def verify_file_map(root, files):
    require(isinstance(files, dict) and files, 'missing immutable file manifest')
    for name, sha in files.items():
        require(digest(selected_path(root, name)) == sha, f'immutable evidence changed: {name}')


def independent_manifest(manifest, fixture, candidate, binary):
    """Validate B's own released state before interpreting any current proof."""
    require(manifest.get('schema_version') == 1 and
            manifest.get('scope') == 'additional-genuine-060-writer-package-exercise' and
            manifest.get('fixture_root') == str(fixture), 'unrecognized independent fixture')
    source = fixture / manifest['project']
    require(source.resolve().is_relative_to(fixture), 'project escapes independent fixture')
    released, current = manifest['released'], manifest['candidate']
    require(released.get('sha256') == RELEASE_SHA256 and digest(released['path']) == RELEASE_SHA256 and
            released.get('version') == 'fluent 0.6.0 be19732c' and
            read_command([released['path'], 'version'], source) == released['version'],
            'independent released executable changed')
    require(current.get('commit') == candidate and
            digest(current['binary_path']) == current['binary_sha256'] and
            current.get('version') == 'fluent 0.6.0 ' + candidate[:8] and
            read_command([current['binary_path'], 'version'], source) == current['version'] and
            read_command([binary, 'version'], source) == current['version'],
            'independent candidate executable changed')
    snapshot = Path(manifest['snapshot_root']).resolve(strict=True)
    verify_file_map(snapshot, manifest['original_public_snapshot'])
    require(manifest['original_item'] in manifest['original_public_snapshot'], 'original public state is not frozen')
    original = json.loads(selected_path(snapshot, manifest['original_item']).read_text())
    attempt = attempt_from(original, manifest['work'], manifest['attempt'])
    require(original.get('learner_mode') == 'no-expertise', 'independent fixture must retain no-expertise policy')
    initial, failure = manifest['initial'], manifest['failure']
    writers = [t for t in attempt['tasks'] if t['id'] == initial['writer_task_id']]
    require(len(writers) == 1 and writers[0].get('output') == initial['writer_output'] and
            initial['writer_output'].get('commit') == initial['writer_commit'] and
            initial['writer_output'].get('base_commit') == initial['seed_commit'],
            'independent initial Writer identity changed')
    require_registered_writer_commit(source, manifest['work'], attempt,
                                     dict(producer_id=writers[0]['id'], path=initial['writer_commit']))
    writes = writers[0].get('workspace_access', {}).get('writes', [])
    require(len(writes) == 1 and (source / writes[0]['path']).resolve() ==
            Path(initial['assigned_workspace']).resolve(), 'initial assigned workspace changed')
    require(read_command(['git', '--no-replace-objects', 'rev-parse', '--verify',
                          'refs/tags/' + initial['host_tag'] + '^{commit}'], source) == initial['writer_commit'],
            'initial host tag changed')
    failed = [t for t in attempt['tasks'] if t['id'] == failure['task_id']]
    require(len(failed) == 1 and failed[0].get('kind') == 'write' and failed[0].get('status') == 'failed' and
            not failed[0].get('output') and failed[0].get('work_item_id') == manifest['work'] and
            failed[0].get('attempt_id') == manifest['attempt'] and
            failure['artifact_path'] == str(Path(failed[0]['artifact_area']['path']) / 'transcript.jsonl') and
            attempt.get('pause_record') == failure.get('public_pause') and
            attempt.get('status') == 'needs-user' and isinstance(failure.get('public_pause'), dict) and
            failure['public_pause'].get('task-id') == failure['task_id'] and
            failure['public_pause'].get('candidate-commit') == initial['writer_commit'] and
            str(uuid.UUID(failure['session_id'])) == failure['session_id'] and
            any(r.get('task_id') == initial['writer_task_id'] and r.get('provider') == 'claude' and
                r.get('candidate_commit') == initial['writer_commit'] and
                r.get('session_id') == failure['session_id'] for r in attempt.get('writer_runs', [])),
            'failure does not belong to the genuine released Writer continuation')
    raw = selected_path(source, failure['artifact_path']).read_bytes()
    result = json.loads(raw)
    require(hashlib.sha256(raw).hexdigest() == failure['artifact_sha256'] and
            failure['artifact_sha256'] in manifest['original_public_snapshot'].values() and
            failure.get('num_turns') == 0 and result.get('num_turns') == 0 and
            result.get('type') == 'result' and result.get('subtype') == 'error_during_execution' and
            result.get('is_error') is True and result.get('session_id') == failure['session_id'] and
            result.get('errors') == [f"No conversation found with session ID: {failure['session_id']}"],
            'missing genuine independent zero-turn failure')
    configuration = manifest['configuration']
    require({'.fluent/config.yaml', '.fluent/tester.yaml'} <= set(configuration), 'missing original configuration')
    require(all(manifest['original_public_snapshot'].get(name) == sha for name, sha in configuration.items()),
            'configuration is absent from the original snapshot')
    verify_file_map(source, configuration)
    # Reuse the original continuation/ownership checks with dynamic identities;
    # retain the entire supplied manifest in the journal to detect later edits.
    return dict(fixture=str(fixture), project=manifest['project'], work=manifest['work'], attempt=manifest['attempt'],
                initial_commit=initial['writer_commit'], missing_session=failure['session_id'],
                released_sha256=released['sha256'], released_binary=released['path'],
                prerequisites=manifest['prerequisites'], prerequisites_sha256=manifest['prerequisites_sha256'],
                snapshot_root=str(snapshot), snapshot_files=manifest['original_public_snapshot'],
                config=str(Path(manifest['project']) / '.fluent/config.yaml'), config_sha256=configuration['.fluent/config.yaml'],
                original_failure=str(Path(manifest['project']) / failure['artifact_path']),
                original_failure_sha256=failure['artifact_sha256'],
                stage2_marker=manifest['stage2_marker'], stage2_marker_sha256=manifest['stage2_marker_sha256'])


def require_frozen_candidate(source, task, initial, tip):
    """Compare the first regression tree with the actual clean producing workspace."""
    writes = task.get('workspace_access', {}).get('writes', [])
    require(len(writes) == 1 and writes[0].get('path'), 'missing assigned final workspace')
    workspace = (source / writes[0]['path']).resolve(strict=True)
    require(read_command(['git', '--no-replace-objects', 'rev-parse', 'HEAD'], workspace) == tip,
            'assigned final workspace does not contain the producing tip')
    require(not read_command(['git', 'status', '--porcelain=v1', '--untracked-files=all'], workspace),
            'assigned final workspace must be clean')
    trees = []
    for commit in (initial, tip):
        entries = read_command(['git', '--no-replace-objects', 'ls-tree', '-rz', '--full-tree', commit], workspace)
        tree = {}
        for entry in entries.split('\0'):
            if entry:
                identity, path = entry.split('\t', 1)
                tree[path] = identity.split(' ')
        trees.append(tree)
    baseline, final = trees
    require('story.test.js' in baseline and baseline.keys() == final.keys(),
            'frozen first regression paths changed')
    for path, identity in baseline.items():
        require(final[path][:2] == identity[:2] and
                (path in ('story.js', 'README.md') or final[path] == identity),
                'frozen first regression input changed: ' + path)


def affected_review_roles(paths):
    """Map changed paths to the ordinary review domains they invalidate."""
    roles = {'architecture', 'behaviors', 'documentation', 'skills', 'tests'}
    affected = set()
    for raw in paths:
        path = Path(raw)
        if not raw or path.is_absolute() or any(part in ('.', '..') for part in raw.split('/')):
            return roles
        if raw.startswith('.fluent/expertise/'):
            continue
        if raw in ('documentation/architecture.md', 'expertise/architecture.md'):
            affected.update(('documentation', 'architecture'))
        elif raw in ('documentation/behaviors.md', 'expertise/behaviors.md'):
            affected.update(('documentation', 'behaviors', 'tests'))
        elif raw.startswith('documentation/') or raw == 'README.md' or raw == 'LICENSE' or raw.startswith('CHANGELOG'):
            affected.add('documentation')
        elif raw == 'expertise/tests.md':
            affected.update(('documentation', 'tests'))
        elif raw == 'expertise/skills.md':
            affected.update(('documentation', 'skills'))
        elif raw in ('expertise/documentation.md', 'expertise/README.md'):
            affected.add('documentation')
        elif raw.startswith('expertise/'):
            return roles
        elif raw.startswith('skills/'):
            affected.update(('documentation', 'skills'))
        elif raw.startswith('skill-migrations/'):
            affected.update(('documentation', 'skills', 'tests'))
        elif raw.startswith('prompts/'):
            affected.update(('documentation', 'behaviors', 'skills', 'tests'))
        elif raw.startswith('tests/'):
            affected.add('tests')
        elif (raw.startswith(('src/', 'scripts/', 'tools/', 'infrastructure/', 'sandboxes/', '.fluent/hooks/'))
              or raw in ('.fluent/tester.yaml', '.fluent/extract-tester-results', 'Cargo.toml', 'Cargo.lock', 'build.rs')):
            affected.update(('documentation', 'architecture', 'tests'))
        else:
            return roles
    return affected


def changed_review_paths(workspace, before, after):
    """Read both sides of renames so a moved file cannot retain stale coverage."""
    result = read_command(['git', '--no-replace-objects', 'diff', '--name-status', '-z', '--find-renames',
                           '--diff-filter=ACDMRTUXB', before, after], workspace).split('\0')
    paths = []
    index = 0
    while index < len(result) and result[index]:
        status = result[index]
        count = 2 if status.startswith(('R', 'C')) else 1
        require(status[0] in 'ACDMRTUXB' and len(result) > index + count,
                'ordinary review change list is malformed')
        paths.extend(result[index + 1:index + count + 1])
        index += count + 1
    return paths


def independent_ordinary_reviews(source, attempt):
    """Select authentic passing reviews after each Writer's domain changes."""
    roles = {'architecture', 'behaviors', 'documentation', 'skills', 'tests'}
    selected = {}
    current = None
    for task in attempt['tasks']:
        if task.get('status') != 'complete':
            continue
        if task.get('kind') == 'write':
            output = task.get('output') or {}
            commit = output.get('commit')
            require(commit, 'independent Writer commit is missing')
            if current and current != commit:
                workspace = (source / task['workspace_access']['writes'][0]['path']).resolve(strict=True)
                changed = changed_review_paths(workspace, current, commit)
                for role in affected_review_roles(changed):
                    selected.pop(role, None)
            current = commit
        elif task.get('kind') == 'review':
            role = task.get('role')
            if role not in roles:
                selected.clear()
                continue
            if task.get('evidence_review_context'):
                continue
            selected.pop(role, None)
            if current and task.get('review_context', {}).get('candidate_commit') == current:
                selected[role] = task
    return selected


def require_independent_completion(source, item, attempt, manifest, tip):
    require(item.get('learner_mode') == 'no-expertise', 'independent Learning policy changed')
    writers = [t for t in attempt['tasks'] if t.get('kind') == 'write']
    require(writers and writers[-1].get('output', {}).get('commit') == tip,
            'independent ready candidate differs from the producing Writer')
    require_frozen_candidate(source, writers[-1], manifest['initial']['writer_commit'], tip)
    failure = manifest['failure']
    operations = [op for op in attempt.get('recovery_operations', [])
                  if op.get('operation-kind') == 'claude-session-retry' and op.get('task-id') == failure['task_id']]
    require(len(operations) == 1, 'missing bounded public missing-session recovery')
    recovered = [t for t in attempt['tasks'] if t['id'] == failure['task_id']]
    require(len(recovered) == 1, 'recovered Writer Task changed')
    require_package_artifact_assignment(source, attempt, recovered[0])
    reference = next(r for r in recovered[0]['input_artifacts'] if r['producer_id'] == 'claude-session-retry')
    audit = json.loads(selected_path(source, reference['path']).read_text())
    snapshot = Path(manifest['snapshot_root'])
    original = json.loads(selected_path(snapshot, manifest['original_item']).read_text())
    old = attempt_from(original, manifest['work'], manifest['attempt'])
    require([t for t in audit['before']['tasks'] if t['id'] == failure['task_id']] ==
            [t for t in old['tasks'] if t['id'] == failure['task_id']] and
            audit.get('files', {}).get(failure['artifact_path']) == 'sha256:' + failure['artifact_sha256'],
            'public recovery lost the original failure identity')
    reviews = independent_ordinary_reviews(source, attempt)
    for role in ('architecture', 'behaviors', 'documentation', 'skills', 'tests'):
        require(role in reviews, 'independent ordinary review is missing or stale')
        task = reviews[role]
        path = str(Path(task['artifact_area']['path']) / 'review.md')
        require(dict(producer_id=task['id'], path=path) in attempt['artifacts'] and
                selected_path(source, path).read_text().splitlines()[:1] == ['Verdict: pass'],
                'independent ordinary review did not pass')


def released_fixture(args, commands, candidate, binary):
    require(args.fixture is not None and args.fixture_manifest is not None,
            'root must explicitly supply --fixture and --fixture-manifest')
    fixture = args.fixture.resolve(strict=True)
    supplied = json.loads(args.fixture_manifest.read_text())
    independent = supplied.get('scope') == 'additional-genuine-060-writer-package-exercise'
    if independent:
        manifest = independent_manifest(supplied, fixture, candidate, binary)
    else:
        manifest = supplied
        require(manifest.get('fixture') == str(fixture) and fixture.name == 'real-060-node-claude-upgrade',
                'unrecognized frozen fixture')
        require(manifest.get('initial_commit') == INITIAL_COMMIT and
                manifest.get('missing_session') == MISSING_SESSION and
                manifest.get('released_sha256') == RELEASE_SHA256, 'frozen release identity changed')
    initial, session = manifest['initial_commit'], manifest['missing_session']
    verify_files = verify_file_map if independent else verify_snapshot
    prerequisite = Path(manifest['prerequisites'])
    require(digest(prerequisite) == manifest['prerequisites_sha256'], 'prerequisite receipt changed')
    require_complete(json.loads(prerequisite.read_text()), candidate)
    snapshot_root = Path(manifest['snapshot_root']).resolve(strict=True)
    verify_files(snapshot_root, manifest['snapshot_files'])
    require(digest(Path(manifest['released_binary'])) == RELEASE_SHA256, 'released executable changed')
    # Root advances its existing marker separately. This driver never writes it.
    marker = selected_path(fixture, manifest['stage2_marker'])
    require(digest(marker) == manifest['stage2_marker_sha256'], 'root must advance the existing stage-2 marker')
    config = selected_path(fixture, manifest['config'])
    require(digest(config) == manifest['config_sha256'], 'project configuration changed')
    failure = selected_path(fixture, manifest['original_failure'])
    require(digest(failure) == manifest['original_failure_sha256'], 'original failure changed')
    original = json.loads(failure.read_text())
    require(original.get('session_id') == session and original.get('num_turns') == 0 and
            any('No conversation found' in e for e in original.get('errors', [])), 'missing genuine zero-turn result')
    source = fixture / manifest.get('project', 'main')
    require(source.resolve().is_relative_to(fixture), 'project escapes frozen fixture')
    before = show(binary, source, manifest['work'])
    original_attempt = attempt_from(before, manifest['work'], manifest['attempt'])
    require_released_ownership(before, manifest['work'], source)
    if independent and not args.verify_only:
        require(original_attempt.get('status') != 'complete' and
                original_attempt.get('learning', {}).get('status') != 'succeeded' and
                not any(m.get('attempt_id') == manifest['attempt'] and m.get('ready_at')
                        for m in before.get('merge_candidates', [])),
                'completed independent exercise cannot be continued or relabeled on another candidate')
    require(any(t.get('output', {}).get('commit') == initial
                for t in original_attempt['tasks'] if isinstance(t.get('output'), dict)),
            'the retained first Writer commit does not belong to this Attempt')
    commands.run('verify-retained-commit', ['git', 'cat-file', '-e', initial + '^{commit}'], cwd=source)
    receipt = dict(candidate=candidate, binary_sha256=digest(binary), fixture=str(fixture),
                   initial_commit=initial, missing_session=session, result='incomplete')
    if args.verify_only:
        require(args.continuation_output is not None, '--verify-only requires --continuation-output')
        prior = args.continuation_output.resolve(strict=True)
        saved = json.loads(selected_path(prior, 'continuation.json').read_text())
        require(saved.get('schema_version') == 1 and saved.get('candidate') == candidate and
                saved.get('fixture') == str(fixture) and saved.get('work') == manifest['work'] and
                saved.get('attempt') == manifest['attempt'] and saved.get('invoked') is True,
                'continuation receipt belongs to another candidate or fixture')
        for name, sha in saved['files'].items():
            require(digest(selected_path(prior, name)) == sha, 'continuation evidence changed')
        require(json.loads(selected_path(prior, 'frozen-manifest.json').read_text()) == supplied,
                'frozen manifest changed after continuation')
        history = json.loads(selected_path(prior, 'retained-history.json').read_text())
        configuration = saved['configuration']
        require(installed_npm() == saved['npm'], 'installed npm changed since continuation')
        receipt['continuation'] = dict(path=str(prior / 'continuation.json'), sha256=digest(prior / 'continuation.json'))
        receipt['npm'] = saved['npm']
    else:
        history = retained_history(source, before, manifest['work'], manifest['attempt'])
        configuration = {str(p.relative_to(source)): digest(p) for p in (source / '.fluent').glob('*.yaml')}
        npm = installed_npm()
        # This is the host driver's journal, never a Fluent execution record.
        # A retry validates the retained run; only a new candidate may continue again.
        journal = fixture / '.scratch' / f'integrated-upgrade-{candidate}.json'
        journal.parent.mkdir(exist_ok=True)
        require(not journal.exists(), 'fixture already continued on this candidate; use --verify-only')
        write_json(commands.output / 'fixture-before.json', before)
        write_json(commands.output / 'frozen-manifest.json', supplied)
        write_json(commands.output / 'retained-history.json', history)
        saved = dict(schema_version=1, candidate=candidate, binary_sha256=digest(binary), fixture=str(fixture),
                     work=manifest['work'], attempt=manifest['attempt'], npm=npm, configuration=configuration,
                     invoked=True, files={name:digest(commands.output / name) for name in
                         ('fixture-before.json', 'frozen-manifest.json', 'retained-history.json')})
        with journal.open('x') as stream:
            json.dump(dict(output=str(commands.output), candidate=candidate), stream)
        write_json(commands.output / 'continuation.json', saved)
        receipt['npm'] = npm
    completed = False
    try:
        if not args.verify_only:
            commands.run('public-resume', [binary, 'attempt', 'run', manifest['work'], manifest['attempt']],
                         cwd=source, timeout=7200)
            receipt['result'] = 'continued'
        else:
            attempt = attempt_from(before, manifest['work'], manifest['attempt'])
            ready = [m for m in before.get('merge_candidates', [])
                     if m.get('attempt_id') == manifest['attempt'] and m.get('ready_at')
                     and m.get('merge_state', {}).get('status') == 'pending']
            require(len(ready) == 1, 'fixture has not reached a ready Merge Candidate; retain history and follow public recovery guidance')
            tip = ready[0]['candidate_commit']
            commands.run('preserve-initial-ancestry', ['git', 'merge-base', '--is-ancestor', initial, tip], cwd=source)
            require(attempt.get('learning', {}).get('status') == 'succeeded', 'Learning did not succeed')
            require(attempt.get('status') == 'complete' and attempt.get('review_state') == 'passed',
                    'ordinary review or completion gates remain pending')
            if independent:
                require_independent_completion(source, before, attempt, supplied, tip)
            verify_packages(args, source, attempt, tip, commands)
            completed = True
            receipt['merge_candidate'] = ready[0]
    finally:
        after = show(binary, source, manifest['work'])
        write_json(commands.output / 'fixture-after.json', after)
        write_json(commands.output / 'released-upgrade.json', receipt)
        clean_candidate(commands.root, candidate)
        verify_files(snapshot_root, manifest['snapshot_files'])
        require(digest(config) == manifest['config_sha256'] and
                digest(failure) == manifest['original_failure_sha256'], 'retained fixture history changed')
        verify_retained_history(source, after, history)
        require(all(digest(selected_path(source, p)) == sha for p, sha in configuration.items()), 'configuration bytes changed')
        attempt_from(after, manifest['work'], manifest['attempt'])
        require_released_ownership(after, manifest['work'], source)
        if args.verify_only:
            require(after == before, 'fixture changed during verify-only')
        if completed:
            receipt['result'] = 'pass'
        write_json(commands.output / 'released-upgrade.json', receipt)
    return receipt


def historical_exercise(path, binary, output):
    """Audit A's pinned bytes and live retained records without continuing it.

    The approved failed envelope is a provenance anchor, never current-candidate
    acceptance. Its nonterminal unused reservations must remain exactly as saved.
    """
    raw = selected_path(path.parent, path.name).read_bytes()
    require(hashlib.sha256(raw).hexdigest() == HISTORICAL_ENVELOPE_SHA256,
            'historical exercise envelope changed')
    envelope = json.loads(raw)
    require(envelope.get('check') == HOST_CHECKS['released'][1] and envelope.get('result') == 'fail',
            'historical failure was relabeled')
    payload = json.loads(envelope['output'])
    archive = payload['archive']
    compressed = base64.b64decode(archive['data'], validate=True)
    require(hashlib.sha256(compressed).hexdigest() == archive['xz_sha256'], 'historical archive changed')
    decoded = lzma.decompress(compressed)
    require(len(decoded) == archive['uncompressed_bytes'] and
            hashlib.sha256(decoded).hexdigest() == archive['uncompressed_sha256'], 'historical archive bytes changed')
    archive = json.loads(decoded)
    files = {}
    for entry in archive['manifest']:
        contents = base64.b64decode(archive['blobs_base64'][entry['sha256']], validate=True)
        require(len(contents) == entry['bytes'] and hashlib.sha256(contents).hexdigest() == entry['sha256'] and
                entry['path'] not in files, 'historical embedded file changed')
        files[entry['path']] = contents

    def embedded(suffix):
        matches = [value for name, value in files.items() if name.endswith(suffix)]
        require(len(matches) == 1, 'historical evidence identity is ambiguous or missing')
        return json.loads(matches[0])

    prefix = '/6167-continuation-host2/host-output/'
    manifest = embedded(prefix + 'frozen-manifest.json')
    saved = embedded(prefix + 'continuation.json')
    executable = embedded(prefix + 'binary.json')
    after = embedded(prefix + 'fixture-after.json')
    history = embedded(prefix + 'retained-history.json')
    source = Path(manifest['fixture']) / manifest['project']
    require(saved.get('candidate') == HISTORICAL_CANDIDATE and saved.get('invoked') is True and
            executable.get('candidate') == HISTORICAL_CANDIDATE and
            executable.get('version') == 'fluent 0.6.0 ' + HISTORICAL_CANDIDATE[:8] and
            saved.get('binary_sha256') == executable['sha256'] and
            digest(executable['path']) == executable['sha256'] and
            read_command([executable['path'], 'version'], source) == executable['version'],
            'historical continuation executable changed')
    require(manifest.get('initial_commit') == INITIAL_COMMIT and manifest.get('missing_session') == MISSING_SESSION and
            manifest.get('released_sha256') == RELEASE_SHA256 and
            digest(manifest['released_binary']) == RELEASE_SHA256 and
            saved.get('fixture') == manifest['fixture'] and saved.get('work') == manifest['work'] and
            saved.get('attempt') == manifest['attempt'], 'historical fixture identity changed')
    for name, sha in saved['files'].items():
        matches = [value for key, value in files.items() if key.endswith(prefix + name)]
        require(len(matches) == 1 and hashlib.sha256(matches[0]).hexdigest() == sha, 'historical continuation bytes changed')
    verify_snapshot(Path(manifest['snapshot_root']), manifest['snapshot_files'])
    for field in ('config', 'original_failure', 'stage2_marker'):
        require(digest(selected_path(Path(manifest['fixture']), manifest[field])) == manifest[field + '_sha256'],
                'historical fixture bytes changed')
    verify_file_map(source, saved['configuration'])
    export = embedded('/fluent-row4-public-completed-proof-6167/manifest.json')
    for entry in export['manifest']:
        relative = Path(entry['source']).relative_to(source)
        contents = selected_path(source, relative).read_bytes()
        require(len(contents) == entry['bytes'] and hashlib.sha256(contents).hexdigest() == entry['sha256'],
                'historical public artifact changed')
    current = show(binary, source, manifest['work'])
    # Metrics and guidance are computed by the reader; compare durable public
    # state and separately hash every exported Task, result and execution file.
    for key in ('id', 'learner_mode', 'attempts', 'merge_candidates'):
        require(current.get(key) == after.get(key), 'historical public state changed')
    verify_retained_history(source, current, history)
    target = attempt_from(current, manifest['work'], manifest['attempt'])
    ready = [m for m in current.get('merge_candidates', []) if m.get('attempt_id') == target['id'] and m.get('ready_at')]
    require(len(ready) == 1 and ready[0]['candidate_commit'] == export['candidate'] and
            target.get('status') == 'complete' and target.get('learning', {}).get('status') == 'succeeded' and
            current.get('learner_mode') == 'no-expertise', 'historical ready state changed')
    read_command(['git', '--no-replace-objects', 'merge-base', '--is-ancestor', INITIAL_COMMIT,
                  ready[0]['candidate_commit']], source)
    ownership = embedded('/6167-continuation-host2/ownership-diagnostic.json')['attempts']
    gaps = {}
    for attempt in current['attempts']:
        owner = OwnedAttempt([binary, 'attempt', 'run', manifest['work'], attempt['id']], source, {})
        snapshot = owner.snapshot(current)
        require(owner.settled(snapshot) and snapshot['executions'] == ownership[attempt['id']]['executions'] and
                snapshot['setup'] == ownership[attempt['id']]['setup'], 'historical ownership records changed')
        gaps.update({name: e['record'] for name, e in snapshot['executions'].items()
                     if e['record'].get('state') not in ('settled', 'canceled') or not e['record'].get('settled_at')})
    require(len(gaps) == 2 and all(r.get('state') == 'launching' and r.get('leader_pid') == 0 and
                                 r.get('process_group_id') == 0 and not r.get('settled_at') for r in gaps.values()),
            'historical unused reservations were relabeled')
    receipt = dict(scope='historical-only', result='retained-with-gaps', fixture=manifest['fixture'],
                   work=manifest['work'], attempt=manifest['attempt'], candidate=HISTORICAL_CANDIDATE,
                   binary=executable, initial_commit=INITIAL_COMMIT, missing_session=MISSING_SESSION,
                   merge_candidate=ready[0], snapshot_files=manifest['snapshot_files'],
                   nonterminal_unused_reservations=gaps,
                   package_gap='Final Writer lacks standalone npm ci and npm test success; Tester success is separate.',
                   envelope_sha256=HISTORICAL_ENVELOPE_SHA256)
    (output / 'historical-envelope.json').write_bytes(raw)
    write_json(output / 'historical-exercise.json', receipt)
    return receipt


def released_aggregate(args, commands, candidate, binary):
    require(getattr(args, 'historical_evidence', None) is not None,
            'select the approved historical envelope with --historical-evidence')
    historical = historical_exercise(args.historical_evidence, binary, commands.output)
    manifest = json.loads(args.fixture_manifest.read_text())
    require(manifest.get('scope') == 'additional-genuine-060-writer-package-exercise',
            'aggregate requires an independent current exercise')
    fixture = args.fixture.resolve(strict=True)
    old = Path(historical['fixture']).resolve(strict=True)
    require(not fixture.is_relative_to(old) and not old.is_relative_to(fixture) and
            manifest.get('work') != historical['work'] and
            manifest['failure']['session_id'] != historical['missing_session'],
            'independent exercise reuses historical identity')
    try:
        fresh = released_fixture(args, commands, candidate, binary)
    finally:
        require(historical_exercise(args.historical_evidence, binary, commands.output) == historical,
                'historical exercise changed during independent verification')
    receipt = dict(schema_version=1, candidate=candidate, result=fresh['result'],
                   historical=historical, current=fresh)
    write_json(commands.output / 'integrated-released-upgrade.json', receipt)
    return receipt


def main():
    require(os.environ.get('FLUENT_INTEGRATED_UPGRADE_HOST') == '1',
            'FLUENT_INTEGRATED_UPGRADE_HOST=1 is required in the root-owned host stage')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('prerequisites', 'released'))
    for name, variable in [('source-root', 'FLUENT_INTEGRATED_SOURCE_ROOT'),
                           ('output', 'FLUENT_INTEGRATED_OUTPUT'),
                           ('fixture', 'FLUENT_INTEGRATED_FIXTURE'),
                           ('fixture-manifest', 'FLUENT_INTEGRATED_FIXTURE_MANIFEST')]:
        parser.add_argument('--' + name, type=Path, default=os.environ.get(variable),
                            required=name in ('source-root', 'output') and variable not in os.environ)
    parser.add_argument('--verify-only', action='store_true',
                        default=os.environ.get('FLUENT_INTEGRATED_VERIFY_ONLY') == '1', help='validate a retained continuation without invoking attempt run')
    parser.add_argument('--continuation-output', type=Path, default=os.environ.get('FLUENT_INTEGRATED_CONTINUATION_OUTPUT'))
    parser.add_argument('--package-proofs', type=Path, default=os.environ.get('FLUENT_INTEGRATED_PACKAGE_PROOFS'))
    parser.add_argument('--historical-evidence', type=Path, default=os.environ.get('FLUENT_INTEGRATED_HISTORICAL_EVIDENCE'),
                        help='the approved failed envelope for historical exercise A; never resumed')
    parser.add_argument('--work', default=os.environ.get('FLUENT_INTEGRATED_WORK'),
                        required='FLUENT_INTEGRATED_WORK' not in os.environ)
    parser.add_argument('--attempt', default=os.environ.get('FLUENT_INTEGRATED_ATTEMPT', 'attempt-1'))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    candidate = clean_candidate(root)
    require(os.uname().sysname == 'Darwin', 'real Seatbelt host is required')
    require(not any(k.startswith('FLUENT_TEST_') for k in os.environ) and
            'FLUENT_BIN_OVERRIDE' not in os.environ, 'remove synthetic host overrides')
    output = args.output.resolve()
    require(not output.is_relative_to(root), 'evidence must be outside the candidate')
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    global READ_DIAGNOSTICS
    READ_DIAGNOSTICS = output / 'reads'
    # Serialize this driver's invocations without taking ownership of Fluent's
    # execution records. Public commands keep their own authoritative leases.
    lock_path = root / '.scratch/integrated-upgrade-host.lock'
    lock_path.parent.mkdir(exist_ok=True)
    with lock_path.open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        commands = Commands(root, output, dict(os.environ, FLUENT_NO_UPDATE_CHECK='1'))
        try:
            binary = production_binary(commands)
            before = host_snapshot(binary, args, root, candidate)
            if args.mode == 'prerequisites':
                prerequisites(args, commands, candidate, binary)
            else:
                require(args.fixture is not None, 'root must explicitly supply --fixture')
                # Two candidate checkouts must not consume the same retained
                # fixture concurrently. The public coordinator still owns its
                # execution leases; this additional lock only owns this driver.
                fixture = args.fixture.resolve(strict=True)
                scratch = fixture / '.scratch'
                scratch.mkdir(exist_ok=True)
                with (scratch / 'integrated-upgrade-driver.lock').open('a') as fixture_lock:
                    fcntl.flock(fixture_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    receipt = released_aggregate(args, commands, candidate, binary)
            require(host_snapshot(binary, args, root, candidate) == before,
                    'paused Work changed during host checks')
            if args.mode == 'released':
                require(receipt['result'] == 'pass',
                        'continuation retained; select package evidence and use --verify-only with --continuation-output and --package-proofs')
        finally:
            write_json(output / 'commands.json', commands.records)
            write_json(output / 'sha256.json', direct_evidence(output))
    print(f'ok integrated upgrade {args.mode}; evidence: {output}')


if __name__ == '__main__':
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f'error: {error}', file=sys.stderr)
        sys.exit(1)
