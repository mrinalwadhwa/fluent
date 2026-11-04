#!/bin/sh
# Succeed only when an existing file rejects a real update attempt.
set -eu
target=$1
touch_program=${2:-/usr/bin/touch}
test -f "$target" || exit 2
test -x "$touch_program" || exit 3
if "$touch_program" "$target" 2>/dev/null; then
    exit 4
fi
