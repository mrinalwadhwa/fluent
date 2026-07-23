---
name: atomic-write-replace-through-utility
description: Replace a durable file through crate::atomic_write::atomic_write (unique temp + persist), never a fixed-name temp + rename; and distinguish NotFound from other read errors instead of unwrap_or_default
metadata:
  type: convention
---

Replacing a durable file in place goes through
`crate::atomic_write::atomic_write` (a unique `NamedTempFile::new_in` +
`persist`), the utility used at ~15 sites across the executor modules. Do **not**
hand-roll a fixed-name temp file (`progress.md.materialize.tmp`) plus
`fs::rename` — a fixed temp name collides under concurrent materialization, and
the reviewer flags any re-implementation of the atomic-replace primitive.

Paired requirement: when you read a file you may then replace, **distinguish
`io::ErrorKind::NotFound` from every other read error**. `NotFound` maps to
empty/absent; a UTF-8, permission, or other IO error must propagate with context.
Reading with `fs::read_to_string(&p).unwrap_or_default()` folds an unreadable
file into "empty" and then clobbers the existing-but-unreadable file with a fresh
render. The audit requirement is: `NotFound` → empty, any other read error →
propagate, then write through `atomic_write`.

This is the overwrite-safe replacement path, distinct from exclusive-create
evidence writes ([[host-evidence-writes-use-exclusive-create]]), which use
`create_new(true)` because they must *never* overwrite. Pick `atomic_write` when
replacing a host-owned file; pick `create_new` when the record must be immutable.
