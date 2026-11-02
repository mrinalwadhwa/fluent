#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/fluent}"
source "$PROJECT_DIR/tests/lib/run_test.sh"
LOG_DIR="$PROJECT_DIR/tests/output/$(basename "$0" .sh)"

test_ordinary_tester_excludes_release_host_checks() {
  # Check the real declarations; execute small suites in a disposable checkout.
  python3 - "$PROJECT_DIR" <<'PY' || return 1
from pathlib import Path
import sys
root = Path(sys.argv[1])
ordinary = (root / '.fluent/tester.yaml').read_text()
host = (root / '.fluent/release-host.yaml').read_text()
assert ordinary.count('  - command:') == 2
assert 'tests/host/check-' not in ordinary
assert host.count('  - command: bash tests/host/check-') == 7
assert 'cargo nextest run --features test-support' in ordinary
assert 'tests/behaviors/operations/*.sh tests/behaviors/skills/*.sh' in ordinary
PY
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  mkdir -p "$tmp/.fluent"
  cp "$PROJECT_DIR/.fluent/release-host.yaml" "$tmp/.fluent/"
  cp "$PROJECT_DIR/.fluent/extract-tester-results" "$tmp/.fluent/"
  cat > "$tmp/.fluent/tester.yaml" <<'YAML'
commands:
  - command: printf 'ok rust-suite\n'
    test_harness: shell-harness
  - command: printf 'ok shell-suite\n'
    test_harness: shell-harness
YAML
  git -C "$tmp" init -q
  git -C "$tmp" config user.name test
  git -C "$tmp" config user.email test@example.invalid
  git -C "$tmp" add .fluent
  git -C "$tmp" commit -qm 'Seed ordinary Tester fixture'
  (cd "$tmp" && "$FLUENT_BIN" --no-sandbox tester check) > "$tmp/result" 2>&1 || { cat "$tmp/result"; return 1; }
  cat "$tmp/result"
}
run_test ordinary_tester_excludes_release_host_checks test_ordinary_tester_excludes_release_host_checks
summarize_and_exit
