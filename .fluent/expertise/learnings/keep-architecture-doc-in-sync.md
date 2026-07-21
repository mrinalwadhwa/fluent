---
name: keep-architecture-doc-in-sync
description: documentation/architecture.md is a living present-tense doc; subsystem changes must update its file-map lines and subsystem sections in the same change
metadata:
  type: convention
---

`documentation/architecture.md` is a living document written in present tense. It contains two things a subsystem change can invalidate:

1. A `src/` **file map** — one line per source file describing what it does (e.g. `queue.rs` → "Per-Work-Item dispatch ledger (history + one active dispatch)").
2. Per-subsystem **prose sections** describing the current model — data shapes, status vocabularies, state transitions, and workflow semantics.

When a Work Item replaces or reshapes a subsystem, both must be updated so the doc describes the shipped system, not the one the change deleted. The documentation reviewer **blocks** when the living doc still describes deleted behavior in present tense — an inaccurate schema, status set, or workflow that would mislead a contributor or operator is a shipping blocker. In one run this was the sole blocking finding: the doc still described the old single-entry queue and sequential scheduler after they were replaced by a dispatch ledger and elected coordinator.

Rewrites should read as connected prose grouping related behaviors, not as a restated list of EARS statements from `behaviors.md`, and must avoid AI writing tells.

If keeping the doc in sync is deliberately deferred to a later slice, record that as an explicit decision so the reviewer does not flag it.

Related: [[behaviors-test-citation-sync]]
