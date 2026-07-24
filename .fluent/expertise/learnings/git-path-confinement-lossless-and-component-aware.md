---
name: git-path-confinement-lossless-and-component-aware
description: Classify Git paths an untrusted coder produced from raw -z NUL output keeping the bytes (never newline-delimited --name-only), and gate a directory boundary component-aware, never with starts_with
metadata:
  type: gotcha
---

When the host must decide whether the paths an untrusted coder wrote are confined
to a directory boundary (e.g. the Learner's `.fluent/expertise/`), two lossy
shortcuts silently break confinement, and the reviewers here treat closing both as
the point of the change.

**Read paths losslessly.** Inventory paths from raw NUL-delimited Git output
(`diff-tree ... -z`, `diff --name-only -z`, `ls-files --others -z`) and keep the
raw bytes (`Vec<Vec<u8>>`), deferring UTF-8 decoding and classification to a
single later pass. The default newline-delimited `--name-only` is unsafe: a
newline-bearing filename like `.fluent/expertise/a.md\nnot-expertise` splits into
a legitimate path plus a forged out-of-bounds fragment, so either the whole
result is wrongly rejected or a smuggled path slips the boundary. A non-UTF-8
path must be *rejected*, not `String::from_utf8_lossy`-decoded — a lossy decode
invents a path that neither matches the boundary nor round-trips into the
persisted reference model. Split raw output on the NUL byte only, dropping empty
trailing records, and classify each byte string whole.

**Gate the boundary component-aware, not by prefix.** `path.starts_with(".fluent/
expertise/")` accepts `.fluent/expertise-notes/x` and depends on trailing-slash
accidents. Split on `/` and require the exact leading components (`.fluent`, then
`expertise`), at least one further component (reject the bare directory), and no
empty, `.`, or `..` component (reject absolute paths and traversal). A lexical
near-miss must never be accepted as in-bounds.

The adversarial tests that lock this in author non-representable paths through Git
plumbing rather than the filesystem — a non-UTF-8 name is staged with
`update-index --index-info` bytes on stdin and committed via `write-tree` /
`commit-tree` / `update-ref`, because APFS refuses the on-disk filename. See
[[host-owned-git-transaction-over-untrusted-coder]] for the transaction this
classification runs inside.
