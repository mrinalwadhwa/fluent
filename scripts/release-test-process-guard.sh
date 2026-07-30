#!/usr/bin/env bash
# Run one release suite and reject Fluent/provider processes that survive below
# a fixture root. Tests may inject a deterministic inventory when the host
# sandbox denies process-table access.
set -euo pipefail

roots="${FLUENT_RELEASE_TEST_ROOTS:?set FLUENT_RELEASE_TEST_ROOTS to fixture roots}"
inventory="${FLUENT_TEST_PROCESS_INVENTORY:-}"

snapshot() {
  if [ -n "$inventory" ]; then
    [ -f "$inventory" ] && cat "$inventory"
    return 0
  fi

  { /bin/ps -axo pid=,command= 2>/dev/null || true; } | while IFS= read -r line; do
    pid="${line%% *}"
    command="${line#"$pid"}"
    case "$command" in
      *fluent*|*claude*|*codex*|*" pi "*) ;;
      *) continue ;;
    esac
    IFS=: read -r -a root_list <<< "$roots"
    for root in "${root_list[@]}"; do
      case "$command" in
        *"$root"*) printf '%s\t%s\t%s\n' "$pid" "process" "$root"; break ;;
      esac
    done
  done
}

before="$(mktemp)"
after="$(mktemp)"
cleanup() { rm -f "$before" "$after"; }
trap cleanup EXIT
snapshot | sort > "$before"

if "$@"; then
  command_result=0
else
  command_result=$?
fi

for _ in 1 2 3 4 5; do
  snapshot | sort > "$after"
  if ! comm -13 "$before" "$after" | grep -q .; then
    exit "$command_result"
  fi
  sleep 0.1
done

echo "release test process leak(s):" >&2
comm -13 "$before" "$after" | while IFS=$'\t' read -r pid kind root; do
  echo "  pid=$pid kind=$kind root=$root" >&2
done
exit 1
