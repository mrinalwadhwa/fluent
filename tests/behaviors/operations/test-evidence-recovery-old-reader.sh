#!/bin/bash
# Prove that the pre-change executable cannot discard required-check authority.
set -euo pipefail
repository=$(git rev-parse --show-toplevel)
current_bin="${FLUENT_BIN_OVERRIDE:-$repository/target/debug/fluent}"
old_bin=$(bash "$repository/tests/fixtures/build-evidence-old-reader.sh")
export LOG_DIR="$repository/tests/output/test-evidence-recovery-old-reader"
source "$repository/tests/lib/run_test.sh"
test_evidence_fence() {
python3 - "$current_bin" "$old_bin" <<'PY'
import hashlib, json, shutil, subprocess, sys, tempfile
from pathlib import Path
current, old = map(Path, sys.argv[1:])
identity = json.loads((old.parents[2] / 'identity.json').read_text())
assert identity['source_commit'] == '720368bb855dd6d10fffd221ed0e4badd0b6104e'
assert identity['binary_sha256'] == hashlib.sha256(old.read_bytes()).hexdigest()
print(json.dumps(identity, sort_keys=True))
def run(binary, root, *args):
    return subprocess.run([binary, *args], cwd=root, capture_output=True, text=True, timeout=30)
def success(result):
    assert result.returncode == 0, result.stdout + result.stderr
def snapshot(root):
    return [(str(p.relative_to(root)), 'directory' if p.is_dir() else hashlib.sha256(p.read_bytes()).hexdigest()) for p in sorted(root.rglob('*'))]
with tempfile.TemporaryDirectory(prefix='fluent-evidence-fence-') as temporary:
    root = Path(temporary) / 'project'
    root.mkdir()
    success(run(current, root, 'work-item', 'create', 'evidence-fence', '--title', 'Preserve evidence'))
    success(run(current, root, 'attempt', 'create', 'evidence-fence', 'attempt-1'))
    work = root / '.fluent/work'
    item_path = work / 'items/evidence-fence.json'
    attempt_path = work / 'attempts/evidence-fence/attempt-1.json'
    item = json.loads(item_path.read_text())
    assert item['schema_version'] == 13
    # Seed storage authority, not acceptance eligibility. Production CLI tests
    # establish eligibility; this fixture detects fields discarded by writers.
    attempt = json.loads(attempt_path.read_text())
    area = 'artifacts/evidence-fence/attempt-1/host-evidence'
    artifacts = {}
    def bound(name, content):
        path = work / area / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        artifacts[path] = content
        return {'path': '.fluent/work/' + area + '/' + name,
                'digest': 'sha256:' + hashlib.sha256(content).hexdigest()}
    result = bound('result.json', b'{"result":"pass"}\n')
    audit = bound('audit.json', b'{"source":"preserved canonical run"}\n')
    review = bound('review.md', b'Verdict: pass\n')
    required = {
        'id': 'tester:fixture:0', 'work_item_id': 'evidence-fence',
        'attempt_id': 'attempt-1', 'candidate_commit': '1' * 40,
        'command': 'true', 'working_directory': '.',
        'applicability': {'definition': {'environment': {'os': 'linux'},
            'comparison_refs': ['main'], 'freshness': {'kind': 'candidate-code'}},
            'comparison_commits': {'main': '2' * 40}},
        'source': {'kind': 'canonical-tester', 'task_id': 'attempt-1-tester',
            'command_index': 0, 'results': result, 'manifest': audit,
            'reconstruction': audit, 'logs': [], 'comparison_commit': '2' * 40},
    }
    recovery = {
        'id': 'host-evidence-fixture', 'candidate_commit': '1' * 40,
        'required_check': required, 'attachment': {
            'snapshot_path': result['path'], 'digest': result['digest']},
        'targets': [{'role': 'tests', 'prior_review_artifact': audit['path'],
            'review_task_id': 'attempt-1-review-2-tests', 'review_digest': review['digest']}],
        'state': 'complete', 'created_at': '2026-09-09T12:00:00Z',
    }
    attempt['evidence_recoveries'] = [recovery]
    attempt_path.write_text(json.dumps(attempt))
    success(run(current, root, 'attempt', 'cancel', 'evidence-fence', 'attempt-1'))
    assert json.loads(attempt_path.read_text())['evidence_recoveries'] == [recovery]
    assert all(path.read_bytes() == content for path, content in artifacts.items())
    # Prove the pinned executable can execute this mutation on its own format.
    control = Path(temporary) / 'control'
    control.mkdir()
    success(run(old, control, 'work-item', 'create', 'legacy', '--title', 'Control'))
    success(run(old, control, 'work-item', 'abandon', 'legacy', '--reason', 'positive control'))
    for pending in (False, True):
        if pending:
            # Interrupt an actual public write after its journal is durable.
            # Readers can still read the Tasks; atomic replacement cannot write.
            task_dir = work / 'tasks/evidence-fence/attempt-1'
            previous_mode = task_dir.stat().st_mode
            task_dir.chmod(0o555)
            try:
                interrupted = run(current, root, 'work-item', 'abandon',
                    'evidence-fence', '--reason', 'exercise interrupted storage')
            finally:
                task_dir.chmod(previous_mode)
            assert interrupted.returncode != 0, interrupted.stdout + interrupted.stderr
            journal = work / 'transactions/evidence-fence.json'
            transaction = json.loads(journal.read_text())
            assert transaction['schema_version'] == 13
            assert transaction['work_item']['attempts'][0]['evidence_recoveries'] == [recovery]
        for command in [('work-item', 'abandon', 'evidence-fence', '--reason', 'must refuse'), ('work-item', 'upgrade', 'evidence-fence')]:
            before = snapshot(work)
            refused = run(old, root, *command)
            assert refused.returncode != 0, refused.stdout + refused.stderr
            assert 'schema' in refused.stderr or 'transaction' in refused.stderr, refused.stderr
            assert snapshot(work) == before, f'older mutation changed evidence state (pending={pending})'
        print(f'pending={pending}: old public mutations refused with unchanged bytes and entries')
    success(run(current, root, 'work-item', 'upgrade', 'evidence-fence'))
    assert not journal.exists()
    assert json.loads(attempt_path.read_text())['evidence_recoveries'] == [recovery]
    assert all(path.read_bytes() == content for path, content in artifacts.items())
    print('Current public journal recovery retained check policy, sources, review digest and audit bytes')
PY
}
run_test 'Pre-change reader refuses current evidence authority and journals' test_evidence_fence
summarize_and_exit
