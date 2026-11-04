#!/usr/bin/env bash
# Prepare the pinned reader from before Tester permission evidence existed.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=330bdba6281a394580da0a0194b3cd8d0fcb8295
reader_root="$repository/.scratch/tester-permission-old-reader/$source_commit"
helper_dir="$(cd "$(dirname "$0")" && pwd)"
python3 "$helper_dir/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root"
