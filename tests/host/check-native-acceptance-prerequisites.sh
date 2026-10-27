#!/usr/bin/env bash
# A separate required host check; never run this from a Writer Task.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
exec python3 -I tests/fixtures/native-acceptance.py
