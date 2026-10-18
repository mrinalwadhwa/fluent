#!/usr/bin/env bash
set -euo pipefail

repository="$(git rev-parse --show-toplevel)"
REAL_GIT="$(command -v git)"
export REAL_GIT
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
mkdir -p "$temporary/bin" "$temporary/cache" "$temporary/cargo-home" "$temporary/cargo-cwd" "$temporary/one" "$temporary/two"
cat > "$temporary/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1-}" == --version ]]; then echo "${CARGO_VERSION:-cargo fixture}"; exit; fi
[[ "${1-}" == build ]] || exit 99
printf 'build\n' >> "$BUILD_LOG"
if [[ "${FAIL_BUILD:-0}" == 1 ]]; then echo 'fixture build failed' >&2; exit 1; fi
if [[ -n "${BUILD_STARTED:-}" ]]; then
  touch "$BUILD_STARTED"
  until [[ -e "$BUILD_RELEASE" ]]; do sleep 0.01; done
fi
if [[ -n "${EXPECT_CARGO_CWD:-}" && "$(pwd -P)" != "$(cd "$EXPECT_CARGO_CWD" && pwd -P)" ]]; then
  echo "cargo ran from $PWD, expected $EXPECT_CARGO_CWD" >&2
  exit 1
fi
if [[ -n "${EXPECT_CARGO_HOME:-}" && "$(cd "${CARGO_HOME:-/}" && pwd -P)" != "$(cd "$EXPECT_CARGO_HOME" && pwd -P)" ]]; then
  echo "cargo used ${CARGO_HOME:-unset}, expected $EXPECT_CARGO_HOME" >&2
  exit 1
fi
while (( $# )); do
  if [[ "$1" == --target-dir ]]; then target="$2"; break; fi
  shift
done
mkdir -p "$target/debug"
if [[ "${OMIT_BINARY:-0}" == 1 ]]; then exit 0; fi
printf '#!/bin/sh\necho fixture\n' > "$target/debug/fluent"
chmod +x "$target/debug/fluent"
SH
chmod +x "$temporary/bin/cargo"
cat > "$temporary/bin/rustc" <<'SH'
#!/usr/bin/env bash
echo "${RUSTC_VERSION:-rustc fixture}"
SH
chmod +x "$temporary/bin/rustc"
cat > "$temporary/bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1-}" == archive && "${TAMPER_LOCK:-0}" == 1 ]]; then
    "$REAL_GIT" "$@" | python3 -c '
import io, sys, tarfile
source = tarfile.open(fileobj=io.BytesIO(sys.stdin.buffer.read()))
output = tarfile.open(fileobj=sys.stdout.buffer, mode="w|")
for member in source:
    content = source.extractfile(member).read() if member.isfile() else None
    if member.name == "Cargo.lock":
        content += b"\n# distinct lockfile\n"
        member.size = len(content)
    output.addfile(member, io.BytesIO(content) if content is not None else None)
output.close()
'
else
    exec "$REAL_GIT" "$@"
fi
SH
chmod +x "$temporary/bin/git"
export PATH="$temporary/bin:$PATH" BUILD_LOG="$temporary/builds"
export CARGO_HOME="$temporary/cargo-home"
export TMPDIR="$temporary/cargo-cwd"
export EXPECT_CARGO_CWD="$TMPDIR"
export FLUENT_HISTORICAL_READER_CACHE="$temporary/cache"
commit=720368bb855dd6d10fffd221ed0e4badd0b6104e
helper="$repository/tests/fixtures/prepare-historical-reader.py"
python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/first"
python3 "$helper" "$repository" "$commit" "$temporary/two" > "$temporary/second"
(( $(wc -l < "$BUILD_LOG") == 1 ))
cmp "$(cat "$temporary/first")" "$(cat "$temporary/second")"
test -L "$(cat "$temporary/second")"
initial_entry="$(readlink "$(cat "$temporary/first")")"
cp "$(dirname "$initial_entry")/provenance.json" "$temporary/initial-provenance.json"

verify_reader() {
    python3 - "$1" "$2" "$3" "$temporary/initial-provenance.json" "$repository" <<'PY'
import copy, hashlib, io, json, os, pathlib, subprocess, sys, tarfile
path = pathlib.Path(pathlib.Path(sys.argv[1]).read_text().strip())
expected_entry = pathlib.Path(sys.argv[2])
field = sys.argv[3]
baseline = json.loads(pathlib.Path(sys.argv[4]).read_text())['identity']
repository = pathlib.Path(sys.argv[5])
assert path.is_symlink() and path.is_file() and os.access(path, os.X_OK)
assert path.resolve() == expected_entry.resolve()
recorded = json.loads((expected_entry.parent / 'provenance.json').read_text())
identity = recorded['identity']
key = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
assert expected_entry.parent.name == key or expected_entry.parent.name.startswith(key + '.v')
assert recorded['binary_sha256'] == hashlib.sha256(path.read_bytes()).hexdigest()
expected = copy.deepcopy(baseline)
if field == 'rustc':
    expected['rustc'] = 'rustc concurrent\n'
    expected['build_options']['RUSTC_VERSION'] = 'rustc concurrent'
elif field == 'build_options':
    expected['build_options']['RUSTFLAGS'] = '-Copt-level=1'
elif field == 'cargo':
    expected['cargo'] = 'cargo changed\n'
elif field in ('source_commit', 'lockfile_sha256'):
    commit = ('2e88c3cbc811a83d733106ded22d29d716cf7184'
              if field == 'source_commit' else baseline['source_commit'])
    env = dict(os.environ, TAMPER_LOCK='1') if field == 'lockfile_sha256' else os.environ
    archive = subprocess.check_output(('git', 'archive', commit), cwd=repository, env=env)
    with tarfile.open(fileobj=io.BytesIO(archive)) as source:
        expected['lockfile_sha256'] = hashlib.sha256(source.extractfile('Cargo.lock').read()).hexdigest()
        expected['work_model_sha256'] = hashlib.sha256(source.extractfile('src/work_model.rs').read()).hexdigest()
    expected['source_commit'] = commit
    expected['source_archive_sha256'] = hashlib.sha256(archive).hexdigest()
elif field == 'build_command':
    expected['build_command'] = ['cargo', 'build', '--locked']
elif field == 'cargo_cwd_config':
    config = pathlib.Path(os.environ['TMPDIR']).resolve() / '.cargo/config.toml'
    expected['cargo_cwd_config'][str(config)] = hashlib.sha256(config.read_bytes()).hexdigest()
elif field == 'cargo_config':
    config = pathlib.Path(os.environ['CARGO_HOME']) / 'config.toml'
    expected['cargo_home'] = str(config.parent.resolve())
    expected['cargo_config']['config.toml'] = hashlib.sha256(config.read_bytes()).hexdigest()
else:
    assert field == ''
assert identity == expected, field
PY
}

# Concurrent callers publish one complete entry.
RUSTC_VERSION='rustc concurrent' python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/concurrent-one" &
first_pid=$!
RUSTC_VERSION='rustc concurrent' python3 "$helper" "$repository" "$commit" "$temporary/two" > "$temporary/concurrent-two" &
second_pid=$!
wait "$first_pid"
wait "$second_pid"
(( $(wc -l < "$BUILD_LOG") == 2 ))
cmp "$(cat "$temporary/concurrent-one")" "$(cat "$temporary/concurrent-two")"
verify_reader "$temporary/concurrent-one" "$(readlink "$(cat "$temporary/concurrent-one")")" rustc
[[ "$(readlink "$(cat "$temporary/concurrent-one")")" != "$initial_entry" ]]

# Each input changes the key independently.
RUSTFLAGS='-Copt-level=1' python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/rustflags"
verify_reader "$temporary/rustflags" "$(readlink "$(cat "$temporary/rustflags")")" build_options
[[ "$(readlink "$(cat "$temporary/rustflags")")" != "$initial_entry" ]]
CARGO_VERSION='cargo changed' python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/cargo-version"
verify_reader "$temporary/cargo-version" "$(readlink "$(cat "$temporary/cargo-version")")" cargo
[[ "$(readlink "$(cat "$temporary/cargo-version")")" != "$initial_entry" ]]
other_commit=2e88c3cbc811a83d733106ded22d29d716cf7184
python3 "$helper" "$repository" "$other_commit" "$temporary/one" > "$temporary/source"
verify_reader "$temporary/source" "$(readlink "$(cat "$temporary/source")")" source_commit
[[ "$(readlink "$(cat "$temporary/source")")" != "$initial_entry" ]]
TAMPER_LOCK=1 python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/lockfile"
verify_reader "$temporary/lockfile" "$(readlink "$(cat "$temporary/lockfile")")" lockfile_sha256
[[ "$(readlink "$(cat "$temporary/lockfile")")" != "$initial_entry" ]]
python3 "$helper" "$repository" "$commit" "$temporary/one" online > "$temporary/mode"
verify_reader "$temporary/mode" "$(readlink "$(cat "$temporary/mode")")" build_command
[[ "$(readlink "$(cat "$temporary/mode")")" != "$initial_entry" ]]
(( $(wc -l < "$BUILD_LOG") == 7 ))

# A changed executable or provenance is rebuilt before publication.
first_entry="$initial_entry"
printf 'tampered\n' >> "$first_entry"
python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/rebuilt-binary"
rebuilt_entry="$(readlink "$(cat "$temporary/rebuilt-binary")")"
[[ "$rebuilt_entry" != "$first_entry" ]]
verify_reader "$temporary/rebuilt-binary" "$rebuilt_entry" ''
(( $(wc -l < "$BUILD_LOG") == 8 ))
first_entry="$rebuilt_entry"
python3 - "$first_entry" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1]).with_name('provenance.json')
path.write_text('{}')
PY
python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/rebuilt-provenance"
rebuilt_entry="$(readlink "$(cat "$temporary/rebuilt-provenance")")"
[[ "$rebuilt_entry" != "$first_entry" ]]
verify_reader "$temporary/rebuilt-provenance" "$rebuilt_entry" ''
(( $(wc -l < "$BUILD_LOG") == 9 ))
first_entry="$rebuilt_entry"

# A failed rebuild leaves no usable stale reader; an orphan temporary entry is ignored.
printf 'tampered\n' >> "$first_entry"
mkdir -p "$temporary/cache/.interrupted/entry"
if FAIL_BUILD=1 python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/failure.out" 2> "$temporary/failure.err"; then
    echo 'failed build unexpectedly supplied a reader' >&2
    exit 1
fi
rg -q 'build identity [0-9a-f]{64} failed' "$temporary/failure.err"
rg -q 'fixture build failed' "$temporary/failure.err"
[[ ! -s "$temporary/failure.out" ]]
python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/rebuilt-after-failure"
rebuilt_entry="$(readlink "$(cat "$temporary/rebuilt-after-failure")")"
[[ "$rebuilt_entry" != "$first_entry" ]]
verify_reader "$temporary/rebuilt-after-failure" "$rebuilt_entry" ''
(( $(wc -l < "$BUILD_LOG") == 11 ))

# A successful Cargo exit without an executable must fail preparation.
if RUSTC_VERSION='rustc no binary' OMIT_BINARY=1 python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/missing.out" 2> "$temporary/missing.err"; then
    echo 'missing executable unexpectedly supplied a reader' >&2
    exit 1
fi
rg -q 'build identity [0-9a-f]{64}: executable verification failed' "$temporary/missing.err"
[[ ! -s "$temporary/missing.out" ]]

# Every pinned wrapper uses the same preparation path and returns its legacy
# fixture location with independently checkable compatibility metadata.
git clone --shared --quiet "$repository" "$temporary/worktree"
cp "$repository"/tests/fixtures/build-*-old-reader.sh "$temporary/worktree/tests/fixtures/"
cp "$repository/tests/fixtures/prepare-historical-reader.py" "$temporary/worktree/tests/fixtures/"
(
cd "$temporary/worktree"
for wrapper in tests/fixtures/build-*-old-reader.sh; do
    binary="$(bash "$wrapper")"
    python3 - "$binary" "$wrapper" <<'PY'
import hashlib, json, pathlib, subprocess, sys
binary = pathlib.Path(sys.argv[1])
wrapper = pathlib.Path(sys.argv[2]).name
root = binary.parent.parent.parent
identity = json.loads((root / 'identity.json').read_text())
assert binary.is_symlink() and binary.is_file()
assert identity['binary_sha256'] == hashlib.sha256(binary.read_bytes()).hexdigest()
assert len(identity['source_commit']) == 40
if wrapper == 'build-landing-recovery-old-reader.sh':
    assert identity['source_archive_sha256'] == hashlib.sha256((root / 'source.tar').read_bytes()).hexdigest()
    assert identity['work_model_sha256'] == hashlib.sha256((root / 'source/src/work_model.rs').read_bytes()).hexdigest()
if wrapper == 'build-ownership-old-reader.sh':
    for name in ('Cargo.toml', 'Cargo.lock', 'src/main.rs'):
        committed = subprocess.check_output(('git', 'show', f"{identity['source_commit']}:{name}"))
        assert (root / name).read_bytes() == committed
PY
done
stable_root="$temporary/worktree/.scratch/round-cap-approach-revision-stable-reader/bd59cf35308bf30fa5cbabc05ff5cf69cebfff5a"
assignment_root="$temporary/worktree/.scratch/writer-assignment-main-reader-bd59cf35308bf30fa5cbabc05ff5cf69cebfff5a"
[[ "$(readlink "$stable_root/target/debug/fluent")" != "$(readlink "$assignment_root/target/debug/fluent")" ]]
)

# Linked worktrees use the same project-owned cache without building twice.
git clone --shared --quiet "$repository" "$temporary/project"
git -C "$temporary/project" worktree add --detach --quiet "$temporary/work-one" HEAD
git -C "$temporary/project" worktree add --detach --quiet "$temporary/work-two" HEAD
[[ "$(git -C "$temporary/work-one" rev-parse --path-format=absolute --git-common-dir)" == \
   "$(git -C "$temporary/work-two" rev-parse --path-format=absolute --git-common-dir)" ]]
builds_before="$(wc -l < "$BUILD_LOG")"
(
    unset FLUENT_HISTORICAL_READER_CACHE
    python3 "$helper" "$temporary/work-one" "$commit" "$temporary/work-one/.scratch/old-reader" > "$temporary/work-one-path"
    (( $(wc -l < "$BUILD_LOG") == builds_before + 1 ))
    python3 "$helper" "$temporary/work-two" "$commit" "$temporary/work-two/.scratch/old-reader" > "$temporary/work-two-path"
)
(( $(wc -l < "$BUILD_LOG") == builds_before + 1 ))
[[ "$(readlink "$(cat "$temporary/work-one-path")")" == "$(readlink "$(cat "$temporary/work-two-path")")" ]]
[[ "$(readlink "$(cat "$temporary/work-one-path")")" == *"/project/.fluent/work/cache/historical-readers/"* ]]

# Separate Attempt Cargo homes with no config share the same verified reader.
mkdir -p "$temporary/attempt-one-home" "$temporary/attempt-two-home"
builds_before="$(wc -l < "$BUILD_LOG")"
(
    export FLUENT_HISTORICAL_READER_CACHE="$temporary/attempt-cache"
    CARGO_HOME="$temporary/attempt-one-home" python3 "$helper" "$temporary/work-one" "$commit" "$temporary/attempt-one-reader" > "$temporary/attempt-one-path"
    (( $(wc -l < "$BUILD_LOG") == builds_before + 1 ))
    CARGO_HOME="$temporary/attempt-two-home" python3 "$helper" "$temporary/work-two" "$commit" "$temporary/attempt-two-reader" > "$temporary/attempt-two-path"
)
(( $(wc -l < "$BUILD_LOG") == builds_before + 1 ))
[[ "$(readlink "$(cat "$temporary/attempt-one-path")")" == "$(readlink "$(cat "$temporary/attempt-two-path")")" ]]
python3 - "$(readlink "$(cat "$temporary/attempt-one-path")")" <<'PY'
import json, pathlib, sys
entry = pathlib.Path(sys.argv[1]).parent
identity = json.loads((entry / 'provenance.json').read_text())['identity']
assert identity['cargo_home'] is None
assert identity['cargo_config'] == {}
PY

# Unset CARGO_HOME still reuses the project cache across worktrees.
builds_before="$(wc -l < "$BUILD_LOG")"
(
    unset CARGO_HOME FLUENT_HISTORICAL_READER_CACHE
    one_root="$temporary/work-one/.scratch/implicit-reader"
    two_root="$temporary/work-two/.scratch/implicit-reader"
    EXPECT_CARGO_HOME="$one_root/cargo-home" RUSTC_VERSION='rustc implicit homes' \
        python3 "$helper" "$temporary/work-one" "$commit" "$one_root" > "$temporary/implicit-work-one-path"
    EXPECT_CARGO_HOME="$two_root/cargo-home" RUSTC_VERSION='rustc implicit homes' \
        python3 "$helper" "$temporary/work-two" "$commit" "$two_root" > "$temporary/implicit-work-two-path"
)
if (( $(wc -l < "$BUILD_LOG") != builds_before + 1 )); then
    echo 'unset CARGO_HOME rebuilt the reader in the second worktree' >&2
    exit 1
fi
implicit_one="$(readlink "$(cat "$temporary/implicit-work-one-path")")"
implicit_two="$(readlink "$(cat "$temporary/implicit-work-two-path")")"
if [[ "$implicit_one" != "$implicit_two" || "$implicit_one" != *"/project/.fluent/work/cache/historical-readers/"* ]]; then
    echo 'unset CARGO_HOME did not reuse the project cache' >&2
    exit 1
fi

# The calling worktree's Cargo configuration cannot affect an isolated build.
mkdir -p "$temporary/work-two/.cargo"
printf '[build]\nrustflags = ["-Copt-level=1"]\n' > "$temporary/work-two/.cargo/config.toml"
builds_before="$(wc -l < "$BUILD_LOG")"
(
    unset FLUENT_HISTORICAL_READER_CACHE
    python3 "$helper" "$temporary/work-two" "$commit" "$temporary/work-two/.scratch/old-reader" > "$temporary/work-two-config-path"
)
(( $(wc -l < "$BUILD_LOG") == builds_before ))
verify_reader "$temporary/work-two-config-path" "$(readlink "$(cat "$temporary/work-one-path")")" ''

# Repair keeps every previously returned path alive while a new version builds.
rm -f "$CARGO_HOME/config.toml"
mkdir -p "$temporary/retained-one" "$temporary/retained-two"
RUSTC_VERSION='rustc retained' python3 "$helper" "$repository" "$commit" "$temporary/retained-one" > "$temporary/retained-original"
retained_path="$(cat "$temporary/retained-original")"
retained_entry="$(readlink "$retained_path")"
printf '{}\n' > "$(dirname "$retained_entry")/provenance.json"
builds_before="$(wc -l < "$BUILD_LOG")"
RUSTC_VERSION='rustc retained' BUILD_STARTED="$temporary/repair-started" BUILD_RELEASE="$temporary/repair-release" \
    python3 "$helper" "$repository" "$commit" "$temporary/retained-two" > "$temporary/retained-repaired" &
repair_pid=$!
for _ in {1..500}; do
    [[ -e "$temporary/repair-started" ]] && break
    sleep 0.01
done
if [[ ! -e "$temporary/repair-started" || ! -x "$retained_path" || "$("$retained_path" 2>/dev/null)" != fixture ]]; then
    touch "$temporary/repair-release"
    wait "$repair_pid" || true
    echo 'repair removed an executable path already supplied to a reader' >&2
    exit 1
fi
touch "$temporary/repair-release"
wait "$repair_pid"
repaired_path="$(cat "$temporary/retained-repaired")"
repaired_entry="$(readlink "$repaired_path")"
if [[ "$repaired_entry" == "$retained_entry" || ! -x "$retained_path" || "$("$retained_path")" != fixture ]]; then
    echo 'repair did not retain the prior executable version' >&2
    exit 1
fi
python3 - "$temporary/cache" "$repaired_entry" <<'PY'
import pathlib, sys
cache = pathlib.Path(sys.argv[1])
entry = pathlib.Path(sys.argv[2]).parent
key = entry.name.split('.v', 1)[0]
assert (cache / f'{key}.current').resolve() == entry.resolve()
PY
RUSTC_VERSION='rustc retained' python3 "$helper" "$repository" "$commit" "$temporary/retained-one" > "$temporary/retained-same-root"
if [[ "$retained_path" == "$(cat "$temporary/retained-same-root")" || "$(readlink "$retained_path")" != "$retained_entry" || "$("$retained_path")" != fixture ]]; then
    echo 'preparing the repaired reader changed an earlier returned path' >&2
    exit 1
fi
if (( $(wc -l < "$BUILD_LOG") != builds_before + 1 )); then
    echo 'repair did not reuse the newly published version' >&2
    exit 1
fi

# An unset CARGO_HOME uses an isolated, identified home instead of HOME/.cargo.
mkdir -p "$temporary/implicit-home/.cargo"
printf '[build]\nrustflags = ["-Copt-level=1"]\n' > "$temporary/implicit-home/.cargo/config.toml"
isolated_root="$temporary/isolated-reader"
builds_before="$(wc -l < "$BUILD_LOG")"
(
    unset CARGO_HOME
    export HOME="$temporary/implicit-home"
    export EXPECT_CARGO_HOME="$isolated_root/cargo-home"
    python3 "$helper" "$repository" "$commit" "$isolated_root" > "$temporary/isolated-first"
    readlink "$(cat "$temporary/isolated-first")" > "$temporary/isolated-first-entry"
    (( $(wc -l < "$BUILD_LOG") == builds_before + 1 ))
    printf '[build]\nrustflags = ["-Copt-level=2"]\n' > "$HOME/.cargo/config.toml"
    python3 "$helper" "$repository" "$commit" "$isolated_root" > "$temporary/isolated-second"
    readlink "$(cat "$temporary/isolated-second")" > "$temporary/isolated-second-entry"
    (( $(wc -l < "$BUILD_LOG") == builds_before + 1 ))
    printf '[build]\nrustflags = ["-Copt-level=3"]\n' > "$isolated_root/cargo-home/config.toml"
    python3 "$helper" "$repository" "$commit" "$isolated_root" > "$temporary/isolated-third"
    readlink "$(cat "$temporary/isolated-third")" > "$temporary/isolated-third-entry"
)
(( $(wc -l < "$BUILD_LOG") == builds_before + 2 ))
cmp "$temporary/isolated-first-entry" "$temporary/isolated-second-entry"
[[ "$(cat "$temporary/isolated-first-entry")" != "$(cat "$temporary/isolated-third-entry")" ]]
python3 - "$temporary/isolated-third" "$isolated_root" <<'PY'
import hashlib, json, pathlib, sys
third = pathlib.Path(pathlib.Path(sys.argv[1]).read_text().strip())
root = pathlib.Path(sys.argv[2])
assert third.is_symlink() and third.is_file()
entry = third.resolve().parent
recorded = json.loads((entry / 'provenance.json').read_text())
assert recorded['identity']['cargo_home'] == str((root / 'cargo-home').resolve())
assert recorded['identity']['cargo_config']['config.toml'] == hashlib.sha256((root / 'cargo-home/config.toml').read_bytes()).hexdigest()
assert recorded['binary_sha256'] == hashlib.sha256(third.read_bytes()).hexdigest()
PY

# Configuration at Cargo's actual working directory changes the identity.
mkdir -p "$TMPDIR/.cargo"
printf '[build]\nrustflags = ["-Copt-level=1"]\n' > "$TMPDIR/.cargo/config.toml"
python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/cargo-cwd-config"
verify_reader "$temporary/cargo-cwd-config" "$(readlink "$(cat "$temporary/cargo-cwd-config")")" cargo_cwd_config
[[ "$(readlink "$(cat "$temporary/cargo-cwd-config")")" != "$initial_entry" ]]

# Cargo home configuration also changes the build identity.
rm "$TMPDIR/.cargo/config.toml"
printf '[build]\nincremental = false\n' > "$CARGO_HOME/config.toml"
python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/cargo-home-config"
verify_reader "$temporary/cargo-home-config" "$(readlink "$(cat "$temporary/cargo-home-config")")" cargo_config
[[ "$(readlink "$(cat "$temporary/cargo-home-config")")" != "$initial_entry" ]]

# An included file can change build options without changing the including file.
# Reject both versions before returning a cached reader or starting a build.
printf 'include = ["extra.toml"]\n' > "$TMPDIR/.cargo/config.toml"
builds_before="$(wc -l < "$BUILD_LOG")"
for flags in 1 2; do
    printf '[build]\nrustflags = ["-Copt-level=%s"]\n' "$flags" > "$TMPDIR/.cargo/extra.toml"
    if python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/included.out" 2> "$temporary/included.err"; then
        echo 'included Cargo configuration unexpectedly supplied a reader' >&2
        exit 1
    fi
    rg -q 'Cargo configuration include.*config.toml' "$temporary/included.err"
    [[ ! -s "$temporary/included.out" ]]
done
(( $(wc -l < "$BUILD_LOG") == builds_before ))

rm "$TMPDIR/.cargo/config.toml"
printf 'include = ["extra.toml"]\n' > "$CARGO_HOME/config.toml"
if python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/home-include.out" 2> "$temporary/home-include.err"; then
    echo 'Cargo home include unexpectedly supplied a reader' >&2
    exit 1
fi
rg -q 'Cargo configuration include.*config.toml' "$temporary/home-include.err"
[[ ! -s "$temporary/home-include.out" ]]
(( $(wc -l < "$BUILD_LOG") == builds_before ))
