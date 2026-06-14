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
  status
  dashboard
  land
  cleanup
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

deleted_commands=(
  run
  watch
  summary
  review
  resume
  pull
  shell
)

for command in "${deleted_commands[@]}"; do
  if grep -Eq "^factory ${command}([[:space:]-]|$)" <<<"$reference"; then
    echo "deleted command factory ${command} still present in build-in-the-factory command reference" >&2
    failures=$((failures + 1))
  fi
done

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

for entry in "${work_model_entries[@]}"; do
  if ! grep -Fxq "$entry" <<<"$reference"; then
    echo "missing Work-model command entry: ${entry}" >&2
    failures=$((failures + 1))
  fi
done

# Verify deleted legacy entries are absent
deleted_entries=(
  "factory status --runs"
  "factory run "
  "factory summary"
  "factory watch"
  "factory review"
  "factory pull"
  "factory shell"
  "factory resume"
)

for entry in "${deleted_entries[@]}"; do
  if grep -Fq "$entry" <<<"$reference"; then
    echo "deleted legacy command entry still present: ${entry}" >&2
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
