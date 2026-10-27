#!/usr/bin/env bash
# Root audits the retained historical exercise and continues its independent fixture.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
exec python3 -I -S "$root/tests/fixtures/integrated-upgrade.py" released "$@"
