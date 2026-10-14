#!/usr/bin/env bash
# Build the source immediately before no-change completion retry authority.
set -euo pipefail

repository="$(git rev-parse --show-toplevel)"
source_commit=a0f943e3a4147d8064813b54e987b59f93dbc89e
reader_root="$repository/.scratch/no-change-completion-old-reader/$source_commit"
mkdir -p "$reader_root"
source_dir="$(mktemp -d "$reader_root/source.XXXXXX")"
trap 'rm -rf "$source_dir"' EXIT
git -C "$repository" archive "$source_commit" > "$reader_root/source.tar"
tar -xf "$reader_root/source.tar" -C "$source_dir"
GIT_CEILING_DIRECTORIES="$repository" cargo build --locked \
    --manifest-path "$source_dir/Cargo.toml" --target-dir "$reader_root/target" >&2
python3 - "$reader_root" "$source_commit" "$source_dir" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

root, commit, source = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
binary = root / 'target/debug/fluent'
(root / 'identity.json').write_text(json.dumps({
    'source_commit': commit,
    'source_archive_sha256': hashlib.sha256((root / 'source.tar').read_bytes()).hexdigest(),
    'work_model_sha256': hashlib.sha256((source / 'src/work_model.rs').read_bytes()).hexdigest(),
    'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
}, indent=2) + '\n')
print(binary)
PY
