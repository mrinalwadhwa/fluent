#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=fe6b6f607609fbf424daf6ce2224cd51fdf30f74
reader_root="$repository/.scratch/combined-recovery-base-old-reader-$source_commit"
python3 "$repository/tests/fixtures/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root"
