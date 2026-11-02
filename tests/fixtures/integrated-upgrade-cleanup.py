"""Exercise driver cleanup with a disposable foreground Attempt and inert provider."""
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
from unittest import mock

spec = importlib.util.spec_from_file_location('upgrade', Path(__file__).with_name('integrated-upgrade.py'))
upgrade = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upgrade)


def held(path):
    with path.open() as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        return False


def main():
    binary, project, output, mode = sys.argv[1:]
    project, output = Path(project), Path(output)
    work, attempt, task = 'driver-cleanup', 'attempt-1', 'attempt-1-write-1'
    area = project / '.fluent/work/artifacts' / work / attempt / task
    record_path = area / 'execution.json'
    identity = area / 'execution-owner.lock'
    setup = project / '.fluent/work/locks' / work / (task + '.lock')
    before = None
    coordinator = None
    original_popen = subprocess.Popen

    class RegisteredAttempt(original_popen):
        def wait(self, timeout=None):
            nonlocal before, coordinator
            if self.args[1:3] == ['attempt', 'run'] and before is None:
                coordinator = self
                deadline = time.monotonic() + 60
                while time.monotonic() < deadline:
                    if record_path.exists() and (output / 'provider-ready').exists():
                        value = json.loads(record_path.read_text())
                        if value['state'] == 'running' and held(identity):
                            before = value
                            break
                    if self.poll() is not None:
                        raise AssertionError('foreground Attempt exited before registration')
                    time.sleep(0.02)
                assert before is not None, 'provider did not register'
                assert before['process_group_id'] != self.pid
                assert before['launch_generation'] and held(setup)
                (output / 'registered.json').write_text(json.dumps(before))
                if mode == 'reaped':
                    self.kill()
                    result = super().wait(timeout=5)
                    # The provider does not read stdin or exit on pipe closure.
                    assert held(identity), 'inert worker must outlive its reaped coordinator'
                    return result
                # Registration is the barrier, not a guess about startup time.
                return super().wait(timeout=0.05)
            return super().wait(timeout=timeout)

    commands = upgrade.Commands(project, output, dict(os.environ))
    signal_group = os.killpg
    denied_probe = False

    def checked_signal(group, sig):
        nonlocal denied_probe
        if mode == 'denied-probe' and group == coordinator.pid and not sig and not denied_probe:
            assert coordinator.returncode is not None, 'probe must follow reaping'
            denied_probe = True
            raise PermissionError('injected denied probe after coordinator exit')
        if sig:
            assert coordinator.returncode is None, 'cannot signal a reaped coordinator group'
            state = upgrade.show(binary, project, work)['attempts'][0]
            assert state.get('cancellation', {}).get('settled_at'), 'cancel before forced cleanup'
            assert not held(identity) and not held(setup), 'settle workers and setup before signals'
        return signal_group(group, sig)

    try:
        with mock.patch.object(upgrade.subprocess, 'Popen', RegisteredAttempt), \
                mock.patch.object(upgrade.os, 'killpg', side_effect=checked_signal):
            try:
                commands.run('foreground', [binary, 'attempt', 'run', work, attempt,
                                            '--no-sandbox', '--coder', 'claude'], timeout=60)
            except (RuntimeError, subprocess.TimeoutExpired):
                pass
            else:
                raise AssertionError('injected failure must not receive success credit')
        # All assertions precede fallback cleanup; it cannot supply passing proof.
        item = upgrade.show(binary, project, work)
        current = item['attempts'][0]
        stop = current.get('cancellation', {})
        assert stop.get('requested_at') and stop.get('settled_at'), current
        assert not stop.get('resumed_at')
        assert all(t['status'] != 'executing' for t in current['tasks'])
        assert not held(identity) and not held(setup)
        after = json.loads(record_path.read_text())
        assert after['launch_generation'] == before['launch_generation']
        assert after['state'] in ('canceled', 'settled') and after.get('settled_at')
        receipt = json.loads((output / '00-foreground.json').read_text())
        assert receipt['status'] != 0 and receipt['attempt_cleanup']['settled']
        assert receipt['group_settled'] and 'cleanup_error' not in receipt
        if mode == 'denied-probe':
            assert denied_probe, 'exercise the denied process-group probe'
        assert (output / '00-foreground.log').is_file()
        assert (output / '00-foreground-cancel.log').is_file()
        # Preserve the inspected state before the finally block's fallback.
        (output / 'asserted-before-fallback.json').write_text(json.dumps(item, indent=2))
        print(f'ok {mode}: persisted cancellation, generation released, setup and Tasks settled')
    finally:
        # Only this disposable fixture is eligible for fallback cancellation.
        subprocess.run([binary, 'attempt', 'cancel', work, attempt], cwd=project,
                       check=True, timeout=60)
        if coordinator is not None and coordinator.poll() is None:
            coordinator.kill()
            coordinator.wait(timeout=5)


if __name__ == '__main__':
    main()
