---
name: shell-fakes-inject-malformed-cli-output
description: Shell CLI fakes must inject malformed and incomplete JSON responses to cover jq parse failure paths
metadata:
  type: testing
---

A shell script that reads machine-readable output from an external CLI and parses it with
`jq` has two distinct failure modes: the CLI returns a non-zero exit code (typically
caught), or the CLI returns exit 0 with malformed or structurally incomplete JSON (easy
to miss under `set -e` when using command substitution).

A fake that only emits valid, well-formed JSON for expected outputs cannot verify the
second mode. Add fake modes that return:
- Syntactically invalid JSON (e.g., truncated or garbage output with exit 0)
- Valid JSON that omits the expected fields (e.g., `{}` instead of
  `{"merge_state": {"status": "pending"}}`)

Each of these must route through the relevant `fail_phase` call, and the behavior tests
must assert the phase name, the log path, and that the resume command actually completes
on a subsequent run.

This pattern is separate from [[shell-phase-failure-all-exit-paths]] (which covers
non-zero exits) and specifically addresses exit-0 parse failures that `set -e` alone
does not catch. See also [[explicit-state-allowlist-before-nonidempotent]] for the
related case where `jq` successfully parses a null or unexpected state value.
