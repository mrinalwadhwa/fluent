#!/usr/bin/env bash
# Verify the dashboard's one-way app -> layout/snapshot module boundary.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
CHECK="$ROOT/scripts/check-dashboard-module-boundaries.sh"

if ! "$CHECK" "$ROOT"; then
  echo "dashboard module boundary check rejected the repository" >&2
  exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/src/dashboard"
for import in \
  'use super::render;' \
  'use super::{layout, render};' \
  'use crate::dashboard::{render};'
do
  cp "$ROOT/src/dashboard/app.rs" "$TMP/src/dashboard/app.rs"
  printf '%s\n' "$import" >> "$TMP/src/dashboard/app.rs"

  if "$CHECK" "$TMP" >"$TMP/stdout" 2>"$TMP/stderr"; then
    echo "dashboard module boundary check accepted $import" >&2
    exit 1
  fi

  grep -Fq 'app must not import render' "$TMP/stderr"
done

echo "dashboard module boundary behavior tests passed"
