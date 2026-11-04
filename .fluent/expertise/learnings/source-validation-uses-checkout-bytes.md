---
name: source-validation-uses-checkout-bytes
description: Validate source advances against Git checkout bytes and include landed paths to detect edits hidden from status
metadata:
  type: gotcha
---

The Writer's source-advance guard cannot prove product cleanliness from Git
status alone: `assume-unchanged` and `skip-worktree` can hide working-tree edits.
Validate the union of paths whose snapshot hashes changed and paths changed
between the old and new commits. The second set catches a hidden edit that
restores a landed path to its original bytes, leaving no snapshot difference.

For regular files, compare actual bytes with Git's expected checkout
representation through `git::read_checkout_blob` in `src/git.rs`. It uses
`git cat-file --filters --path` with the original path bytes so attributes,
line-ending conversion, and filters apply at the existing Git command/retry
boundary. Raw stored blobs are insufficient: explicit CRLF checkout rules can
produce a clean file whose bytes differ from its committed LF blob.

Keep symlink validation separate: compare the link target with raw blob bytes
without applying regular-file checkout transformations. Check tree additions
and deletions alongside modified paths.

`writer_source_checkout_crlf_fast_forward_does_not_pause` in
`src/work_task_executor.rs` exercises both a clean converted landing and a
hidden edit after conversion through the real Writer Task route.
`writer_source_checkout_unsafe_drift_needs_user` also covers hidden edits that
restore baseline bytes. Preserve both acceptance and rejection cases when
changing this guard.

Related: [[source-advance-requires-writer-confinement]],
[[git-path-confinement-lossless-and-component-aware]],
[[git-subprocesses-use-central-wrapper]].
