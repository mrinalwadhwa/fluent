#!/usr/bin/env bash
# transcript-pump-expertise-is-current - Verify the recorded transcript-pump
# expertise decision describes the shipped per-capture config and single-coordinator
# design, and no longer prescribes the deleted process-global or split-writer
# mechanisms.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
DECISIONS="$ROOT/.fluent/expertise/decisions.md"

if [ ! -f "$DECISIONS" ]; then
  echo "expertise decisions file is missing: $DECISIONS" >&2
  exit 1
fi

# The decision must describe the shipped design.
for concept in \
  'TranscriptCapture' \
  'StatusCoordinator' \
  'run_captured' \
  'per launch' \
  'first fault'
do
  if ! grep -Fq "$concept" "$DECISIONS"; then
    echo "transcript-pump decision does not describe the shipped design: missing '$concept'" >&2
    exit 1
  fi
done

# The decision must document that the deleted process-global and split-writer
# mechanisms were REPLACED, not present them as the current design.
if ! grep -Fq 'replaced the earlier process-wide installed config' "$DECISIONS"; then
  echo "transcript-pump decision does not record replacing the process-global config" >&2
  exit 1
fi
if ! grep -Fq 'replaced the earlier split of a background' "$DECISIONS"; then
  echo "transcript-pump decision does not record replacing the split status writer" >&2
  exit 1
fi

# The stale prescriptive framings of the removed design must be gone.
for stale in \
  'reads its thresholds from a process-wide installed config' \
  'installs them before launching a coder' \
  'the pump cannot take per-call'; do
  if grep -Fq "$stale" "$DECISIONS"; then
    echo "transcript-pump decision still prescribes the removed design: '$stale'" >&2
    exit 1
  fi
done

echo "ok: transcript-pump expertise decision is current"
