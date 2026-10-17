#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=b5d8bf02b016a3977a08fc713e47f525b895aed0
reader_root="$repository/.scratch/carried-finding-old-reader-$source_commit"
helper_dir="$(cd "$(dirname "$0")" && pwd)"
python3 "$helper_dir/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root"
