---
name: smoke-root-identity-binding
description: A multi-phase manifest must record and verify the smoke root so copied or moved roots are rejected
metadata:
  type: architecture
---

A manifest that persists configuration and stage checkpoints for a stateful multi-phase
workflow must record the absolute path of the designated root directory in a
`smoke_root` field (or equivalent). Every subsequent phase must read back that field
and compare it against the supplied root before proceeding:

```sh
verify_manifest_root() {
    local root="$1" manifest_root
    manifest_root=$(jq -r '.smoke_root' "$(manifest_path "$root")")
    if [ "$manifest_root" != "$root" ]; then
        die "smoke root mismatch: manifest was prepared for '$manifest_root'"
    fi
}
```

Without this check, an operator who copies a prepared root and passes the new path to
`run` or `land` will use the manifest's persisted absolute paths (binary, install
boundary) from the old root while creating project, home, and evidence paths under the
new root, splitting durable state across two locations.

Add a regression test that prepares a root, copies it to a new path, and verifies that
`prepare`, `run`, and `land` all reject the copied root without invoking the underlying
tool. The original root must remain usable after the rejection.
