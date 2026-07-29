---
name: rejection-tests-snapshot-owned-trees
description: A CLI rejection that promises no mutation must snapshot every project and home tree it can manage, byte-for-byte, including pre-existing files and empty directories
metadata:
  type: testing
---

Commands such as `fluent init` can modify more than the current project: they
can create Fluent data and materialize skills beneath the user's home directory.
When a precondition rejects such a command, a few sentinels or assertions that
new paths do not exist cannot prove isolation; they miss mutations to existing
files, hidden paths, empty directories, and a second managed skill root.

Drive the public CLI from the rejected location with an isolated `HOME`. Before
the invocation, seed every project and home-managed tree reachable by that
route, then compare complete deterministic snapshots afterward. A useful
snapshot records directory entries as well as file paths and bytes, sorts
children by name, and uses `symlink_metadata` so it observes hidden and empty
paths without following links. Include each agent skill location the command
manages and Fluent's home data. This makes the no-mutation behavior executable
rather than inferred from the implementation order.

Keep output assertions separate and precise: a rejection may name a valid
location without promising a runnable recovery command. Related:
[[declared-behavior-tests-must-exist-before-land]],
[[next-action-guidance-is-an-executable-interface]],
[[test-names-match-assertions]].
