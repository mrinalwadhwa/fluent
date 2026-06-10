#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/build-in-the-factory/SKILL.md"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-$ROOT/target/debug/factory}"

if [ ! -x "$FACTORY_BIN" ]; then
  (cd "$ROOT" && cargo build --quiet)
fi

extract_reference() {
  awk '
    /^## Factory commands$/ { in_section = 1; next }
    in_section && /^```sh$/ { in_block = 1; next }
    in_block && /^```$/ { exit }
    in_block { print }
  ' "$SKILL"
}

reference="$(extract_reference)"

required_commands=(
  work
  run
  status
  watch
  summary
  dashboard
  resume
  land
  cleanup
  pull
  shell
  init
  version
)

failures=0

for command in "${required_commands[@]}"; do
  if ! grep -Eq "^factory ${command}([[:space:]-]|$)" <<<"$reference"; then
    echo "missing factory ${command} from build-in-the-factory command reference" >&2
    failures=$((failures + 1))
  fi
done

if ! "$FACTORY_BIN" --help | grep -Eq '^  dashboard[[:space:]]'; then
  echo "factory --help did not expose dashboard command" >&2
  failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
