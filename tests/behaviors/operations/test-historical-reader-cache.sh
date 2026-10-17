#!/usr/bin/env bash
set -euo pipefail

repository="$(git rev-parse --show-toplevel)"
REAL_GIT="$(command -v git)"
export REAL_GIT
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
mkdir -p "$temporary/bin" "$temporary/cache" "$temporary/cargo-home" "$temporary/one" "$temporary/two"
cat > "$temporary/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1-}" == --version ]]; then echo "${CARGO_VERSION:-cargo fixture}"; exit; fi
[[ "${1-}" == build ]] || exit 99
printf 'build\n' >> "$BUILD_LOG"
if [[ "${FAIL_BUILD:-0}" == 1 ]]; then echo 'fixture build failed' >&2; exit 1; fi
while (( $# )); do
  if [[ "$1" == --target-dir ]]; then target="$2"; break; fi
  shift
done
mkdir -p "$target/debug"
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
export FLUENT_HISTORICAL_READER_CACHE="$temporary/cache"
commit=720368bb855dd6d10fffd221ed0e4badd0b6104e
helper="$repository/tests/fixtures/prepare-historical-reader.py"
python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/first"
python3 "$helper" "$repository" "$commit" "$temporary/two" > "$temporary/second"
(( $(wc -l < "$BUILD_LOG") == 1 ))
cmp "$(cat "$temporary/first")" "$(cat "$temporary/second")"
test -L "$(cat "$temporary/second")"
initial_entry="$(readlink "$(cat "$temporary/first")")"

# Concurrent callers publish one complete entry.
RUSTC_VERSION='rustc concurrent' python3 "$helper" "$repository" "$commit" "$temporary/one" > "$temporary/concurrent-one" &
first_pid=$!
RUSTC_VERSION='rustc concurrent' python3 "$helper" "$repository" "$commit" "$temporary/two" > "$temporary/concurrent-two" &
second_pid=$!
wait "$first_pid"
wait "$second_pid"
(( $(wc -l < "$BUILD_LOG") == 2 ))
cmp "$(cat "$temporary/concurrent-one")" "$(cat "$temporary/concurrent-two")"

# Each input changes the key independently.
RUSTFLAGS='-Copt-level=1' python3 "$helper" "$repository" "$commit" "$temporary/one" > /dev/null
CARGO_VERSION='cargo changed' python3 "$helper" "$repository" "$commit" "$temporary/one" > /dev/null
other_commit=2e88c3cbc811a83d733106ded22d29d716cf7184
python3 "$helper" "$repository" "$other_commit" "$temporary/one" > /dev/null
TAMPER_LOCK=1 python3 "$helper" "$repository" "$commit" "$temporary/one" > /dev/null
python3 "$helper" "$repository" "$commit" "$temporary/one" online > /dev/null
(( $(wc -l < "$BUILD_LOG") == 7 ))

# A changed executable or provenance is rebuilt before publication.
first_entry="$initial_entry"
printf 'tampered\n' >> "$first_entry"
python3 "$helper" "$repository" "$commit" "$temporary/one" > /dev/null
(( $(wc -l < "$BUILD_LOG") == 8 ))
python3 - "$first_entry" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1]).with_name('provenance.json')
path.write_text('{}')
PY
python3 "$helper" "$repository" "$commit" "$temporary/one" > /dev/null
(( $(wc -l < "$BUILD_LOG") == 9 ))

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
python3 "$helper" "$repository" "$commit" "$temporary/one" > /dev/null
(( $(wc -l < "$BUILD_LOG") == 11 ))

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
