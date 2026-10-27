#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$root"
python3 -I -S tests/fixtures/integrated-upgrade-tests.py
printf 'ok integrated upgrade host admission and evidence\n'
