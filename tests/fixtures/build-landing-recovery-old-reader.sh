#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=dfa960cd3bb7ba2087ab8562c728755e75d37f04
reader_root="$repository/.scratch/landing-recovery-old-reader-$source_commit"
python3 "$repository/tests/fixtures/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root" online
