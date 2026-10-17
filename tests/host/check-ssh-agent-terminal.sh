#!/usr/bin/env bash
# Run only after separate authorization for this host and provider journey.
set -euo pipefail
: "${FLUENT_TEST_REAL_SSH_HOST:?separate host/access and provider-run authorization is required}"
: "${FLUENT_SSH_DISPOSABLE_FIXTURES:?explicit disposable fixture authorization is required}"
test "$FLUENT_TEST_REAL_SSH_HOST" = 1
test "$FLUENT_SSH_DISPOSABLE_FIXTURES" = 1
: "${FLUENT_SSH_HOST_OUTPUT:?choose a new evidence directory outside the candidate checkout}"
: "${FLUENT_SSH_LOCAL_BIN:?provide the production Fluent binary built from this candidate}"
: "${FLUENT_SSH_LOCAL_PROJECT:?provide an authorized local companion project}"
: "${FLUENT_SSH_CODEX_SOURCE:?name the configured disposable Codex SSH source}"
: "${FLUENT_SSH_CLAUDE_SOURCE:?name the configured disposable Claude SSH source}"
: "${FLUENT_SSH_PI_SOURCE:?name the configured disposable Pi SSH source}"
test -t 0 && test -t 1
test "$(uname -s)" = Darwin
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
# The Python driver keeps structured data and remote shell operands separate.
python3 -I tests/fixtures/ssh-host-journey.py
