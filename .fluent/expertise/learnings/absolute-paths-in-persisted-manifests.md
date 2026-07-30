---
name: absolute-paths-in-persisted-manifests
description: Relative paths accepted by one phase must be resolved to absolute before being persisted for later phases
metadata:
  type: gotcha
---

When a shell-based multi-phase workflow accepts a user-supplied relative path (e.g.,
`--installer ./tools/install.sh`) during an early phase (prepare) and stores it in a
manifest for use during a later phase (run), the path must be resolved to an absolute
form at write time.

A relative path that is valid when `prepare` runs can silently resolve to a different
location (or fail to resolve) if the operator invokes the next phase from a different
working directory. The manifest is intended to carry configuration durably across the
phase boundary; storing the original relative form defeats that purpose.

Resolve with `realpath` or equivalent only when the path exists (for local files); leave
URLs and other non-filesystem values unchanged:

```sh
if [ -f "$installer" ]; then
    installer=$(realpath "$installer")
fi
```

Test this by running `prepare` from one directory and `run` from another, verifying that
the installer is found and executed correctly in both cases.
