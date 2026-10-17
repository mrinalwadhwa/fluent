#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=f71d20bb11f241c9ab9ba38b401f22e692d9e19f
reader_root="$repository/.scratch/ownership-old-reader"
helper_dir="$(cd "$(dirname "$0")" && pwd)"
python3 "$helper_dir/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root" online root
