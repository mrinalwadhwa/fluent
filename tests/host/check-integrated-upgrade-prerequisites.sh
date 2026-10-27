#!/usr/bin/env bash
# Root owns this host stage; do not invoke from a Writer Task.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
exec python3 -I -S "$root/tests/fixtures/integrated-upgrade.py" prerequisites "$@"
