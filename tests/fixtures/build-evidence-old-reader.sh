#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=720368bb855dd6d10fffd221ed0e4badd0b6104e
reader_root="$repository/.scratch/evidence-old-reader"
python3 "$repository/tests/fixtures/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root"
