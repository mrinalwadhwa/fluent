#!/bin/bash
# Exercise the production Tester profile on a macOS host with real Seatbelt.
set -euo pipefail

fail() { printf 'host process-table check: %s\n' "$*" >&2; exit 1; }
[[ "$(uname -s)" == Darwin ]] || fail 'macOS is required; no host check was run'
[[ -x /usr/bin/sandbox-exec ]] || fail 'Seatbelt is unavailable'
[[ -x /usr/bin/clang ]] || fail 'the macOS C compiler is unavailable'

candidate_root=$(git rev-parse --show-toplevel)
cd "$candidate_root"
evidence_dir=${FLUENT_HOST_EVIDENCE_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/fluent-process-table-host.XXXXXX")}
mkdir -p "$evidence_dir"
evidence_dir=$(cd "$evidence_dir" && pwd -P)
git rev-parse HEAD > "$evidence_dir/initial-head.txt"
git status --porcelain --untracked-files=no > "$evidence_dir/initial-status.txt"
before_head=$(cat "$evidence_dir/initial-head.txt")
[[ ! -s "$evidence_dir/initial-status.txt" ]] || fail 'candidate has tracked changes'

fixture="$evidence_dir/fixture"
approval_id=
fixture_pid=
watcher_pid=
cleanup() {
    status=$?
    if [[ -n "$watcher_pid" ]]; then
        kill "$watcher_pid" 2>/dev/null || true
        wait "$watcher_pid" 2>/dev/null || true
    fi
    if [[ -n "$fixture_pid" ]]; then
        kill -CONT "$fixture_pid" 2>/dev/null || true
        kill "$fixture_pid" 2>/dev/null || true
        wait "$fixture_pid" 2>/dev/null || true
    fi
    if [[ -n "$approval_id" ]]; then
        (cd "$fixture" && "$candidate_root/target/release/fluent" tester permissions revoke "$approval_id") \
            > "$evidence_dir/revocation.json" 2> "$evidence_dir/revocation.err" || status=1
    fi
    git -C "$candidate_root" rev-parse HEAD > "$evidence_dir/final-head.txt" || status=1
    git -C "$candidate_root" status --porcelain --untracked-files=no \
        > "$evidence_dir/final-status.txt" || status=1
    if [[ "$(cat "$evidence_dir/final-head.txt")" != "$before_head" || \
          -s "$evidence_dir/final-status.txt" ]]; then
        printf 'candidate HEAD or tracked source changed during host check\n' >&2
        status=1
    fi
    if [[ -s "$fixture/process-table-query.stderr" ]]; then
        cat "$fixture/process-table-query.stderr" >&2
    fi
    printf 'Host evidence: %s\n' "$evidence_dir" >&2
    exit "$status"
}
trap cleanup EXIT

{
    printf 'candidate=%s\n' "$before_head"
    printf 'started_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'host=%s\n' "$(sw_vers -productVersion)"
    printf 'architecture=%s\n' "$(uname -m)"
} > "$evidence_dir/identity.txt"

# Build this candidate, with no test-support feature or installed Fluent binary.
cargo build --locked --offline --release > "$evidence_dir/build.log" 2>&1 || fail 'production build failed'
binary="$candidate_root/target/release/fluent"
[[ -x "$binary" ]] || fail 'production binary is missing'
shasum -a 256 "$binary" > "$evidence_dir/binary-sha256.txt"

mkdir -p "$fixture/.fluent"
cp "$binary" "$fixture/fluent"
cp "$candidate_root/tests/fixtures/assert-denied-file-update.sh" "$fixture/assert-denied-file-update.sh"
chmod 755 "$fixture/assert-denied-file-update.sh"
git -C "$fixture" init -q -b main
git -C "$fixture" config user.name 'Fluent host fixture'
git -C "$fixture" config user.email 'host-fixture@example.invalid'
git -C "$fixture" config commit.gpgsign false

# The four-element MIB matches the process enumeration used by the consumer.
# Emit assertions only; do not log the host process table.
cat > "$fixture/probe.c" <<'C'
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/sysctl.h>
#include <sys/types.h>
#include <unistd.h>

static int query_failure(const char *phase, const int mib[4]) {
    int saved = errno;
    fprintf(stderr, "process-table sysctl phase=%s mib=[%d,%d,%d,%d] length=4 errno=%d (%s)\n",
            phase, mib[0], mib[1], mib[2], mib[3], saved, strerror(saved));
    errno = saved;
    return -1;
}

static int query(struct kinfo_proc **entries, size_t *count) {
    int mib[4] = { CTL_KERN, KERN_PROC, KERN_PROC_ALL, 0 };
    size_t bytes = 0;
    if (sysctl(mib, 4, NULL, &bytes, NULL, 0) != 0) return query_failure("size", mib);
    struct kinfo_proc *table = malloc(bytes);
    if (table == NULL) return -2;
    if (sysctl(mib, 4, table, &bytes, NULL, 0) != 0) {
        int saved = errno;
        free(table);
        errno = saved;
        return query_failure("data", mib);
    }
    *entries = table;
    *count = bytes / sizeof(*table);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    if (strcmp(argv[1], "control-denied") == 0) {
        if (argc != 3) return 2;
        pid_t target = (pid_t)atoi(argv[2]);
        if (target <= 1) return 2;
        if (kill(target, SIGSTOP) == 0) return 3;
        return errno == EPERM || errno == EACCES ? 0 : 4;
    }
    struct kinfo_proc *table = NULL;
    size_t count = 0;
    int result = query(&table, &count);
    if (strcmp(argv[1], "denied") == 0) {
        if (result == 0) free(table);
        return result == -1 && (errno == EPERM || errno == EACCES) ? 0 : 5;
    }
    if (strcmp(argv[1], "allowed") != 0 || result != 0) return 6;
    pid_t pid = getpid();
    int saw_self = 0;
    int saw_root = 0;
    for (size_t depth = 0; depth < count && depth < 128; ++depth) {
        struct kinfo_proc *match = NULL;
        for (size_t i = 0; i < count; ++i) {
            if (table[i].kp_proc.p_pid == pid) { match = &table[i]; break; }
        }
        if (match == NULL) { free(table); return 7; }
        if (pid == getpid()) {
            saw_self = 1;
            if (match->kp_eproc.e_ucred.cr_uid != getuid()) { free(table); return 8; }
        }
        if (pid == 1) {
            saw_root = match->kp_eproc.e_ucred.cr_uid == 0;
            break;
        }
        pid = match->kp_eproc.e_ppid;
        if (pid <= 0) { free(table); return 9; }
    }
    free(table);
    return saw_self && saw_root ? 0 : 10;
}
C
/usr/bin/clang -Wall -Wextra -Werror -o "$fixture/probe" "$fixture/probe.c" \
    > "$evidence_dir/probe-build.log" 2>&1 || fail 'process-table probe build failed'

# Render the production worker profile variants and observe real Seatbelt
# denial for the same query before granting a Tester command any capability.
cargo run --locked --offline --release --example probe_worker_process_table_profiles -- \
    "$fixture/probe" "$fixture" "$evidence_dir" \
    > "$evidence_dir/worker-profile-probes.log" 2>&1 || fail 'worker profiles admitted the process-table query'

# A host-owned sibling is the only target of the process-control denial probe.
sleep 300 &
fixture_pid=$!

python3 - "$fixture" "$fixture_pid" <<'PY'
import json, os, pwd, shlex, sys
from pathlib import Path
root = Path(sys.argv[1])
pid = sys.argv[2]
probe = shlex.quote(str(root / 'probe'))
query_diagnostics = shlex.quote(str(root / 'process-table-query.stderr'))
fluent = shlex.quote(str(root / 'fluent'))
write_probe = shlex.quote(str(root / 'assert-denied-file-update.sh'))
approval_alias = shlex.quote(str(root / 'protected-target-alias'))
profile_alias = shlex.quote(str(root / 'selected-profile-alias'))
alternate_home = shlex.quote(str(root / 'alternate-home'))
operator_home = Path(pwd.getpwuid(os.geteuid()).pw_dir).resolve()
authority = shlex.quote(str(operator_home / '.config/fluent/tester-permissions'))
unrelated = shlex.quote(str(operator_home / '.fluent-process-table-unrelated-write'))
# Standalone Tester deletes its temporary command logs. Retain syscall errors
# inside the fixture evidence as well, including when the first assertion fails.
allowed = (f'{probe} allowed 2>> {query_diagnostics} && {probe} control-denied {pid}'
           f' && ! touch {authority}/forged-by-tester && ! touch {unrelated}'
           f' && {write_probe} "$FLUENT_FIXTURE_PROTECTED_TARGET"'
           f' && HOME={alternate_home} {write_probe} "$FLUENT_FIXTURE_PROTECTED_TARGET"'
           f' && ln -s "$FLUENT_FIXTURE_PROTECTED_TARGET" {approval_alias}'
           f' && {write_probe} {approval_alias} && rm {approval_alias}'
           ' && (i=0; until test -s "$FLUENT_FIXTURE_PROFILE_PATH_FILE"; do'
           ' i=$((i+1)); test "$i" -lt 300 || exit 17; sleep 0.1; done;'
           ' profile=$(cat "$FLUENT_FIXTURE_PROFILE_PATH_FILE");'
           f' {write_probe} "$profile"'
           f' && ln -s "$profile" {profile_alias}'
           f' && {write_probe} {profile_alias} && rm {profile_alias})'
           f' && ! {fluent} tester permissions approve --candidate "$FLUENT_FIXTURE_SHA"'
           ' --command-index 0 --capability process-table-read'
           ' --request-digest "$FLUENT_FIXTURE_REQUEST"'
           f' && ! {fluent} tester permissions revoke "$FLUENT_FIXTURE_APPROVAL_ID"')
config = {'commands': [
    {'command': allowed, 'test_harness': 'shell-harness', 'permissions': ['process-table-read']},
    {'command': f'{probe} denied', 'test_harness': 'shell-harness'},
]}
(root / '.fluent/tester.yaml').write_text(json.dumps(config) + '\n')
# Readiness has no executed commands. For either executed run, require every
# command to pass as well as the extractor's own process-table denial.
(root / '.fluent/extract-tester-results').write_text(
    '#!/usr/bin/env python3\n'
    'import json, subprocess, sys\n'
    'from pathlib import Path\n'
    'commands = json.loads((Path(sys.argv[1]) / "commands.json").read_text())\n'
    'if commands:\n'
    f'    result = subprocess.run([{str(root / "probe")!r}, "denied"])\n'
    '    if result.returncode != 0:\n'
    '        sys.exit(result.returncode)\n'
    '    if any(command.get("exit_code") != 0 for command in commands):\n'
    '        sys.exit(1)\n'
    'print("[]")\n')
PY
chmod 755 "$fixture/.fluent/extract-tester-results"
git -C "$fixture" add .fluent/tester.yaml .fluent/extract-tester-results probe.c assert-denied-file-update.sh
git -C "$fixture" commit -q -m 'Add process-table host fixture'
fixture_head=$(git -C "$fixture" rev-parse HEAD)
printf '%s\n' "$fixture_head" > "$evidence_dir/fixture-head.txt"

# No YAML declaration or old result authorizes a launch by itself.
if (cd "$fixture" && "$binary" tester check) > "$evidence_dir/unapproved.log" 2>&1; then
    fail 'unapproved command launched'
fi
grep -q 'unmet prerequisite' "$evidence_dir/unapproved.log" || fail 'missing approval was not identified'

(cd "$fixture" && "$binary" tester permissions show --candidate "$fixture_head" --command-index 0) \
    > "$evidence_dir/request.json" 2> "$evidence_dir/request.err"
request_digest=$(python3 - "$evidence_dir/request.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))['request_digest'])
PY
)
(cd "$fixture" && "$binary" tester permissions approve --candidate "$fixture_head" \
    --command-index 0 --capability process-table-read --request-digest "$request_digest") \
    > "$evidence_dir/approval.json" 2> "$evidence_dir/approval.err"
approval_id=$(python3 - "$evidence_dir/approval.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))['approval_id'])
PY
)
export FLUENT_FIXTURE_SHA="$fixture_head"
export FLUENT_FIXTURE_REQUEST="$request_digest"
export FLUENT_FIXTURE_APPROVAL_ID="$approval_id"
profile_path_file="$fixture/selected-profile-path.txt"
profile_watcher_ready="$fixture/profile-watcher-ready"
export FLUENT_FIXTURE_PROFILE_PATH_FILE="$profile_path_file"
authority_dir=$(python3 - <<'PY'
import os, pwd
from pathlib import Path
print(Path(pwd.getpwuid(os.geteuid()).pw_dir).resolve() / '.config/fluent/tester-permissions')
PY
)
approval_record="$authority_dir/${request_digest#sha256:}.json"
[[ -f "$approval_record" ]] || fail 'protected approval record is missing'
export FLUENT_FIXTURE_PROTECTED_TARGET="$approval_record"
start_profile_watcher() {
    local label=$1
    local profile_evidence="$evidence_dir/selected-profile.sb"
    [[ "$label" == first ]] || profile_evidence="$evidence_dir/selected-profile-$label.sb"
    rm -f "$profile_path_file" "$profile_watcher_ready"
    python3 "$candidate_root/tests/fixtures/watch-tester-process-table-profile.py" \
        "$authority_dir" "$profile_path_file" "$profile_evidence" \
        "$profile_watcher_ready" \
        > "$evidence_dir/profile-watcher-$label.log" 2>&1 &
    watcher_pid=$!
    for ((i=0; i<300; i++)); do
        [[ -s "$profile_watcher_ready" ]] && break
        sleep 0.1
    done
    [[ -s "$profile_watcher_ready" ]] || fail 'profile watcher did not establish its starting snapshot'
}
start_profile_watcher first

(cd "$fixture" && "$binary" tester check) \
    > "$evidence_dir/approved-run.log" 2>&1 || fail 'approved production Tester run failed'
wait "$watcher_pid" || fail 'selected production profile was not captured'
watcher_pid=
python3 "$candidate_root/tests/fixtures/verify-tester-process-table-receipt.py" \
    "$evidence_dir" "$authority_dir" "$fixture_head" "$approval_id" "$request_digest"
receipt_id=$(python3 - "$evidence_dir/observed-permission.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))['receipt_id'])
PY
)
receipt_target="$authority_dir/$receipt_id"
receipt_before=$(shasum -a 256 "$receipt_target")
export FLUENT_FIXTURE_PROTECTED_TARGET="$receipt_target"
start_profile_watcher second
(cd "$fixture" && "$binary" tester check) \
    > "$evidence_dir/receipt-denial-run.log" 2>&1 || fail 'receipt denial production Tester run failed'
wait "$watcher_pid" || fail 'second production profile was not captured'
watcher_pid=
[[ "$(shasum -a 256 "$receipt_target")" == "$receipt_before" ]] || fail 'protected receipt changed'
printf 'finished_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$evidence_dir/identity.txt"
printf 'production Tester process-table acceptance passed\n' > "$evidence_dir/result.txt"
