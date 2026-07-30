---
name: shell-printed-commands-quote-paths
description: Shell paths interpolated into user-facing commands must be escaped with printf %q
metadata:
  type: convention
---

When a shell script prints commands for an operator to copy and run (e.g., an inspection
command or a resume command), every external path substituted into the command string must
be shell-escaped using `printf '%q'` so that the printed command is executable when paths
contain spaces or shell metacharacters.

The `first-run-smoke.sh` script implements this as a one-liner helper:

```sh
shq() { printf '%q' "$1"; }
```

All four argument positions in the handoff output (project dir, HOME, binary path, and
root) must be escaped; a single unquoted substitution breaks the generated command for
any path whose name contains a space.

Tests that verify the printed commands are actionable must use a smoke root that contains
a space and must execute (not merely substring-match) the printed inspection and resume
commands. See also [[shell-phase-failure-all-exit-paths]] for the resume-command
requirement that makes this obligation concrete.
