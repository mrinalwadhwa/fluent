#!/usr/bin/env bash
# Reject the dashboard dependency cycle app -> render -> app.

set -euo pipefail

ROOT="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
APP="$ROOT/src/dashboard/app.rs"
LAYOUT="$ROOT/src/dashboard/layout.rs"

if [ ! -f "$APP" ]; then
  echo "dashboard module boundary check cannot read $APP" >&2
  exit 2
fi
if [ ! -f "$LAYOUT" ]; then
  echo "dashboard module boundary check cannot read $LAYOUT" >&2
  exit 2
fi

if grep -Eq '^[[:space:]]*use[[:space:]]+super::app::' "$LAYOUT"; then
  echo 'forbidden dashboard dependency cycle: layout must not import app' >&2
  exit 1
fi

if grep -Eq '^[[:space:]]*use[[:space:]]+.*((super|crate::dashboard)::render|(super|crate::dashboard)::\{[^}]*render)' "$APP"; then
  echo 'forbidden dashboard dependency cycle: app must not import render' >&2
  exit 1
fi
