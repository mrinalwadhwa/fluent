#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=5235313bf19dfd5ebb47ddfcae81aa4f9eaab0ca
reader_root="$repository/.scratch/writer-assignment-paused-reader-$source_commit"
python3 "$repository/tests/fixtures/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root" online
