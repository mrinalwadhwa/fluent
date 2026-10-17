#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=5c670fdd9eec7eda22e93aee4ea619f1c60f7a70
reader_root="$repository/.scratch/round-cap-approach-revision-assignment-reader/$source_commit"
helper_dir="$(cd "$(dirname "$0")" && pwd)"
python3 "$helper_dir/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root"
