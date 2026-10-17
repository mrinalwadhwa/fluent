#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=2e88c3cbc811a83d733106ded22d29d716cf7184
reader_root="$repository/.scratch/work-model-v0.3.0-reader"
helper_dir="$(cd "$(dirname "$0")" && pwd)"
python3 "$helper_dir/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root"
