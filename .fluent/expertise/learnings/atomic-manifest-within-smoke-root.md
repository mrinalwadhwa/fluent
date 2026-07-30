---
name: atomic-manifest-within-smoke-root
description: Shell atomic manifest writes must allocate the temp file beside the target, not in the global /tmp
metadata:
  type: gotcha
---

A shell script that atomically replaces a JSON manifest by writing to a temp file then
renaming must allocate the temp file in the same directory as the target:

```sh
# Correct: temp is beside the target, rename is atomic within one filesystem
tmp=$(mktemp "${manifest}.XXXXXX")
jq ... > "$tmp" && mv "$tmp" "$manifest"

# Wrong: bare mktemp writes to /tmp, which may be a different filesystem;
# mv becomes a cross-filesystem copy+delete, which is not atomic
tmp=$(mktemp)
```

Two problems with the global-temp pattern:
1. The rename can fail or be non-atomic when source and target span filesystems.
2. The temp file escapes the designated smoke root, violating the one-root clean-room
   boundary.

Additionally, a failed `jq` write to `$tmp` must not silently leave an empty or partial
temp file lying around; pair the write with `|| rm -f "$tmp"` and route the failure
through [[shell-phase-failure-all-exit-paths]]. The temp file must be cleaned up whether
or not the write succeeds so the root stays clean on resume.
