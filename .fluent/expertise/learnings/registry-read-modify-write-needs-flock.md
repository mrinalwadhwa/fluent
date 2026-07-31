---
name: registry-read-modify-write-needs-flock
description: JSON registry read-modify-write operations need flock around the full read-write pair; atomic_write alone only protects the individual write
metadata:
  type: architecture
---

`atomic_write` makes the individual write safe (no partial file), but a
read-then-write pair on a shared registry is still vulnerable to concurrent
interleaving: process A reads the stale registry while process B is mid-write,
then A overwrites B's registration.

The fix is an advisory `flock` on the registry file (or a dedicated lock file)
acquired **before** the read and released **after** the `atomic_write`. The
`atomic_write` utility handles the crash-safe replacement; the `flock` is what
serializes the read-modify-write as a unit.

This matters for any registry JSON that multiple concurrent `fluent` invocations
may modify simultaneously, such as `~/.config/fluent/scheduler/registry.json`.

Related: [[atomic-write-replace-through-utility]] covers the safe replacement
primitive itself.
