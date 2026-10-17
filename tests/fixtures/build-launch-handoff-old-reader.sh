#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=edc0febfba85b91c58ff753aefdc4a4606aaaa6e
reader_root="$repository/.scratch/launch-handoff-old-reader-$source_commit"
helper_dir="$(cd "$(dirname "$0")" && pwd)"
python3 "$helper_dir/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root"
