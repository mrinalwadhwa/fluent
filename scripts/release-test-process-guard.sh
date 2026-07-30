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

  # `ps` provides command arguments for precise classification. Some nested
  # macOS test runners deny process-table enumeration but still permit `lsof`
  # for files below the fixture root, so include that root-scoped inventory as
  # a fallback rather than silently accepting every scoped child.
  candidate_pids="$({ /bin/ps -axo pid= 2>/dev/null || true; }
    IFS=: read -r -a root_list <<< "$roots"
    for root in "${root_list[@]}"; do
      /usr/sbin/lsof -a -d cwd +D "$root" -t 2>/dev/null || true
    done)"
  printf '%s\n' "$candidate_pids" | awk 'NF && !seen[$0]++' | while read -r pid; do
    command="$(/bin/ps -p "$pid" -o command= 2>/dev/null || true)"
    if [ -z "$command" ]; then
      command="$(/usr/sbin/lsof -p "$pid" 2>/dev/null | awk 'NR == 2 { print $1 }')"
    fi
    case "$command" in
      *fluent*|*claude*|*codex*|*/pi\ *|pi\ *|*/pi|pi) ;;
      *) continue ;;
    esac
    IFS=: read -r -a root_list <<< "$roots"
    for root in "${root_list[@]}"; do
      root="$(cd "$root" && pwd -P)"
      cwd="$(/usr/sbin/lsof -a -p "$pid" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p')"
      case "$cwd/" in
        "$root"/*) ;;
        *) continue ;;
      esac
      case "$command" in
        *"fluent scheduler"*|*fluent-scheduler*) kind="fluent scheduler" ;;
        *"fluent auto-merge"*) kind="fluent auto-merge" ;;
        *"fluent post-merge-review"*) kind="fluent post-merge-review" ;;
        *claude*) kind="claude" ;;
        *codex*) kind="codex" ;;
        */pi\ *|pi\ *|*/pi|pi) kind="pi" ;;
        *) kind="fluent" ;;
      esac
      printf '%s\t%s\t%s\n' "$pid" "$kind" "$root"
      break
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
