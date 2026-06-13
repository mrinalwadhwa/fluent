2026-06-11 — Several Fargate Work model fixes landed during the
first end-to-end smoke test and should be folded back into a more
careful design pass:
- Local upload tar must disable bsdtar's macOS metadata
  (`--no-mac-metadata`, `--no-xattrs`, `COPYFILE_DISABLE=1`,
  `--exclude=._*`, `--exclude=.DS_Store`).
- Local upload excludes for `target`, `node_modules`, `.scratch`,
  `.factory/work/runtime`, `.git/lfs` to keep payloads small;
  consider a generic `.gitignore`-aware filter.
- Container `/worktrees` parent must be pre-created with non-root
  ownership in the Dockerfile.
- IAM policy must allow GetObject/PutObject for `work/*` and
  `work-merge/*` prefixes alongside `runs/*`.
- Streaming `aws s3 cp - | tar xf -` was unreliable on the
  chainguard aws-cli; both ends use file-based transfers now.
- Embedded absolute `.git` gitfile paths must be repaired via
  `git worktree repair` after each tar extraction (entrypoint and
  local pull).
- Merge launches must include the candidate sibling worktree in
  the upload because the merge executor cd's into it; otherwise
  the container reports `fatal: cannot change to /worktrees/work-<...>: No such file or directory`.
