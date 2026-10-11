#!/usr/bin/env bash
set -euo pipefail

candidate="$(git rev-parse HEAD)"
printf 'candidate=%s\n' "$candidate"
printf 'tmux=%s\n' "$(tmux -V)"
printf 'host=%s\n' "$(sw_vers -productVersion)"
export FLUENT_TEST_REAL_TERMINAL_HOST=1
unset FLUENT_TEST_HOST_SANDBOX_PREFLIGHT
unset FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC
cargo nextest run --offline --features test-support --test terminal_sessions \
  --success-output immediate host_managed_terminal_acceptance
