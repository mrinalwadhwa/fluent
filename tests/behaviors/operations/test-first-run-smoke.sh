#!/usr/bin/env bash
# Compatibility entry point for the clean-room first-run behavior evidence.
# The Work Item cites this path; keep the test implementation in the existing
# harness suite so the smoke journey has one source of behavior coverage.

set -euo pipefail

# Keep the canonical smoke test independent of inherited update suppression.
unset FLUENT_NO_UPDATE_CHECK

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
exec bash "$SCRIPT_DIR/test-first-run-smoke-harness.sh"
