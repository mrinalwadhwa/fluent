#!/bin/bash
# Build the reader immediately before Tester permission evidence existed.
set -euo pipefail
repository=$(git rev-parse --show-toplevel)
source_commit=330bdba6281a394580da0a0194b3cd8d0fcb8295
reader_root="$repository/.scratch/tester-permission-old-reader/$source_commit"
mkdir -p "$reader_root"
if [[ ! -f "$reader_root/source-commit" ]]; then
    git -C "$repository" archive "$source_commit" | tar -x -C "$reader_root"
    printf '%s\n' "$source_commit" > "$reader_root/source-commit"
fi
if ! cargo build --locked --offline --manifest-path "$reader_root/Cargo.toml" \
    --target-dir "$reader_root/target" > "$reader_root/build.log" 2>&1; then
    cat "$reader_root/build.log" >&2
    exit 1
fi
python3 - "$reader_root" "$source_commit" <<'PY'
import hashlib, json, sys
from pathlib import Path
root = Path(sys.argv[1])
binary = root / 'target/debug/fluent'
(root / 'identity.json').write_text(json.dumps({
    'source_commit': sys.argv[2],
    'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
}))
print(binary)
PY
