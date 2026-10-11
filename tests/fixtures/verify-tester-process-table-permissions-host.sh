#!/bin/bash
# Exercise the production Tester profile on a macOS host with real Seatbelt.
set -euo pipefail

fail() { printf 'host process-table check: %s\n' "$*" >&2; exit 1; }
[[ "$(uname -s)" == Darwin ]] || fail 'macOS is required; no host check was run'
[[ -x /usr/bin/sandbox-exec ]] || fail 'Seatbelt is unavailable'
[[ -x /usr/bin/clang ]] || fail 'the macOS C compiler is unavailable'

candidate_root=$(git rev-parse --show-toplevel)
cd "$candidate_root"
before_head=$(git rev-parse HEAD)
[[ -z "$(git status --porcelain --untracked-files=no)" ]] || fail 'candidate has tracked changes'

evidence_dir=${FLUENT_HOST_EVIDENCE_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/fluent-process-table-host.XXXXXX")}
mkdir -p "$evidence_dir"
evidence_dir=$(cd "$evidence_dir" && pwd -P)
fixture="$evidence_dir/fixture"
approval_id=
fixture_pid=
cleanup() {
    status=$?
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
    printf 'Host evidence: %s\n' "$evidence_dir" >&2
    exit "$status"
}
trap cleanup EXIT

{
    printf 'candidate=%s\n' "$before_head"
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

static int query(struct kinfo_proc **entries, size_t *count) {
    int mib[4] = { CTL_KERN, KERN_PROC, KERN_PROC_ALL, 0 };
    size_t bytes = 0;
    if (sysctl(mib, 4, NULL, &bytes, NULL, 0) != 0) return -1;
    struct kinfo_proc *table = malloc(bytes);
    if (table == NULL) return -2;
    if (sysctl(mib, 4, table, &bytes, NULL, 0) != 0) {
        int saved = errno;
        free(table);
        errno = saved;
        return -1;
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

# A host-owned sibling is the only target of the process-control denial probe.
sleep 300 &
fixture_pid=$!

python3 - "$fixture" "$fixture_pid" <<'PY'
import json, os, pwd, shlex, sys
from pathlib import Path
root = Path(sys.argv[1])
pid = sys.argv[2]
probe = shlex.quote(str(root / 'probe'))
fluent = shlex.quote(str(root / 'fluent'))
operator_home = Path(pwd.getpwuid(os.geteuid()).pw_dir).resolve()
authority = shlex.quote(str(operator_home / '.config/fluent/tester-permissions'))
unrelated = shlex.quote(str(operator_home / '.fluent-process-table-unrelated-write'))
allowed = (f'{probe} allowed && {probe} control-denied {pid}'
           f' && ! touch {authority}/forged-by-tester && ! touch {unrelated}'
           f' && ! {fluent} tester permissions approve --candidate "$FLUENT_FIXTURE_SHA"'
           ' --command-index 0 --capability process-table-read'
           ' --request-digest "$FLUENT_FIXTURE_REQUEST"'
           f' && ! {fluent} tester permissions revoke "$FLUENT_FIXTURE_APPROVAL_ID"')
config = {'commands': [
    {'command': allowed, 'test_harness': 'shell-harness', 'permissions': ['process-table-read']},
    {'command': f'{probe} denied', 'test_harness': 'shell-harness'},
]}
(root / '.fluent/tester.yaml').write_text(json.dumps(config) + '\n')
(root / '.fluent/extract-tester-results').write_text(
    '#!/bin/sh\n' + probe + ' denied || exit $?\nprintf "[]\\n"\n')
PY
chmod 755 "$fixture/.fluent/extract-tester-results"
git -C "$fixture" add .fluent/tester.yaml .fluent/extract-tester-results probe.c
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

(cd "$fixture" && "$binary" tester check) \
    > "$evidence_dir/approved-run.log" 2>&1 || fail 'approved production Tester run failed'
printf 'production Tester process-table acceptance passed\n' > "$evidence_dir/result.txt"
