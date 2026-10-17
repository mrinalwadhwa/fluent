#!/usr/bin/env bash
# Prepare the pinned historical reader and preserve its fixture identity path.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=8ab476f75544016bd45b078c8d17627a439914a9
reader_root="$repository/.scratch/installation-output-free-old-reader-$source_commit"
helper_dir="$(cd "$(dirname "$0")" && pwd)"
python3 "$helper_dir/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root"
