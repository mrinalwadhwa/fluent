---
name: log-append-across-retries
description: Retried operation logs must append with timestamps rather than truncate so prior failures are preserved
metadata:
  type: convention
---

When a multi-phase shell workflow allows retried phases (e.g., `run <root>` after a
previous `run` failed), each attempt at a command must append its output to the existing
log rather than truncate it. Truncation removes the failed command, exit status, and
output from the prior attempt — exactly the evidence the operator needs to diagnose what
went wrong.

Use `>>` with a timestamped separator rather than `>` for retried command logs:

```sh
{ printf '\n--- attempt %s ---\n' "$(date -u +%FT%TZ)"; <command>; } >> "$log" 2>&1
```

Or use a `begin_log` helper that appends a session header before each execution.

Allocate per-attempt log filenames (e.g., `attempt-run-1.log`, `attempt-run-2.log`) if
the log must be unambiguous about which attempt produced which output.

This applies to any log that can be written by more than one invocation across the life
of a smoke root: `install.log`, `attempt-run.log`, `attempt-show.log`, `land.log`.
The failure log must be retained while resumed activity is recorded separately or
appended with a clear separator.
