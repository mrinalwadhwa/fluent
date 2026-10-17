#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=bd59cf35308bf30fa5cbabc05ff5cf69cebfff5a
reader_root="$repository/.scratch/writer-assignment-main-reader-$source_commit"
python3 "$repository/tests/fixtures/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root" online
