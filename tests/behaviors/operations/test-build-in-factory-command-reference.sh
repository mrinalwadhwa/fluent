#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/build-in-the-factory/SKILL.md"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-$ROOT/target/debug/factory}"

if [ ! -x "$FACTORY_BIN" ]; then
  (cd "$ROOT" && cargo build --quiet)
fi

extract_command_section() {
  awk '
    /^## Factory commands$/ { in_section = 1 }
    in_section && /^## / && !/^## Factory commands$/ { exit }
    in_section { print }
  ' "$SKILL"
}

extract_reference() {
  awk '
    /^## Factory commands$/ { in_section = 1; next }
    in_section && /^```sh$/ { in_block = 1; next }
    in_block && /^```$/ { exit }
    in_block { print }
  ' "$SKILL"
}

command_section="$(extract_command_section)"
reference="$(extract_reference)"

required_commands=(
  work
  run
  status
  watch
  summary
  review
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

if ! grep -Fq "Work-model commands are listed first" \
  <<<"$command_section"; then
  echo "command reference section lacks Work-before-legacy grouping prose" >&2
  failures=$((failures + 1))
fi

line_number() {
  awk -v needle="$1" '$0 == needle { print NR; exit }' <<<"$reference"
}

work_model_entries=(
  "factory work create <id> --title <t> # create a stored Work Item"
  "factory work create <id> --title <t> --planning-context-file <path> # load planning context"
  "factory work create <id> --title <t> --brief-file <b> --behaviors-file <beh> --approach-file <a> --plan-file <p> # store approved planning files"
  "factory work create <id> --title <t> --instructions <text> # store prompt text"
  "factory work create <id> --title <t> --instructions-file <path> # load prompt file"
  "factory work list                    # list stored Work Items"
  "factory work show <id>               # show one Work Item as JSON"
  "factory work abandon <id> --reason <text> # mark a stale Work Item abandoned"
  "factory work attempt <id> <attempt>  # add an Attempt with a write Task"
  "factory work attempt run <id> <attempt> # advance an Attempt"
  "factory work review <id> <attempt>   # plan review Tasks"
  "factory work review-codebase <id> <attempt> # add a review-only Attempt"
  "factory work task run <id> <attempt> <task> # run one Task"
  "factory work merge-candidate <id> <candidate> # show a Merge Candidate"
  "factory work merge <id> <candidate>  # execute a Merge Candidate"
  "factory status                       # show Work Items by default"
  "factory dashboard                    # open the live dashboard"
)

legacy_entries=(
  "factory status --runs                # show legacy Runs compatibility view"
  "factory run                          # fallback legacy session loop"
  "factory run --run-id <id>            # target a legacy run"
  "factory run --coder codex            # run legacy path with Codex"
  "factory run --runtime fargate        # run legacy path on Fargate"
  "factory summary                      # summarize one legacy run"
  "factory watch                        # poll status, notify on change"
  "factory review                       # create or reuse a legacy review run"
  "factory pull                         # download legacy workspace from S3"
  "factory shell                        # shell into a legacy remote task"
  "factory resume                       # restart a paused legacy run"
  "factory land                         # land a completed legacy run"
)

first_work_line=""
for entry in "${work_model_entries[@]}"; do
  entry_line="$(line_number "$entry")"
  if [ -z "$entry_line" ]; then
    echo "missing Work-model command entry: ${entry}" >&2
    failures=$((failures + 1))
    continue
  fi

  if [ -z "$first_work_line" ] || [ "$entry_line" -lt "$first_work_line" ]; then
    first_work_line="$entry_line"
  fi
done

first_legacy_line=""
for entry in "${legacy_entries[@]}"; do
  entry_line="$(line_number "$entry")"
  if [ -z "$entry_line" ]; then
    echo "missing legacy command entry: ${entry}" >&2
    failures=$((failures + 1))
    continue
  fi

  if [ -z "$first_legacy_line" ] || [ "$entry_line" -lt "$first_legacy_line" ]; then
    first_legacy_line="$entry_line"
  fi
done

if [ -n "$first_legacy_line" ]; then
  for entry in "${work_model_entries[@]}"; do
    entry_line="$(line_number "$entry")"
    if [ -n "$entry_line" ] && [ "$entry_line" -gt "$first_legacy_line" ]; then
      echo "Work-model command appears after first legacy command: ${entry}" >&2
      failures=$((failures + 1))
    fi
  done
fi

if [ -n "$first_work_line" ]; then
  for entry in "${legacy_entries[@]}"; do
    entry_line="$(line_number "$entry")"
    if [ -n "$entry_line" ] && [ "$entry_line" -lt "$first_work_line" ]; then
      echo "legacy command appears before Work-model block starts: ${entry}" >&2
      failures=$((failures + 1))
    fi
  done
fi

if ! "$FACTORY_BIN" --help | grep -Eq '^  dashboard[[:space:]]'; then
  echo "factory --help did not expose dashboard command" >&2
  failures=$((failures + 1))
fi

work_line="$(grep -n '^factory work create ' <<<"$reference" | head -n1 | cut -d: -f1)"
run_line="$(grep -n '^factory run[[:space:]]' <<<"$reference" | head -n1 | cut -d: -f1)"

if [ -z "$work_line" ] || [ -z "$run_line" ] || [ "$work_line" -ge "$run_line" ]; then
  echo "build-in-the-factory command reference does not list Work commands before legacy run commands" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq "normal path for" "$SKILL" || ! grep -Fq "new delegated work" "$SKILL"; then
  echo "build-in-the-factory command reference lacks Work-model-first grouping text" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq "Legacy run commands follow as compatibility" "$SKILL"; then
  echo "build-in-the-factory command reference lacks explicit legacy grouping text" >&2
  failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
