#!/usr/bin/env bash
# Prepare the last reader that understands the base recovery formats.
set -euo pipefail
repository="$(git rev-parse --show-toplevel)"
source_commit=6d8c9bc6de9b93f57d7aab1c9c6640a86b4bf036
reader_root="$repository/.scratch/claude-session-old-reader/$source_commit"
helper_dir="$(cd "$(dirname "$0")" && pwd)"
python3 "$helper_dir/prepare-historical-reader.py" \
    "$repository" "$source_commit" "$reader_root" online
