---
name: follow-up-journal-schema-boundary
description: Keep post-land journal interpretation in follow_up so cleanup does not become a second schema owner
metadata:
  type: architecture
---

`src/follow_up.rs` owns the post-land operation and journal formats. Other subsystems should ask that module for lifecycle facts instead of parsing `journal.json` themselves.

There is currently one cross-boundary reader to treat carefully: `cleanup.rs::has_incomplete_post_land_operation` reads the journal's raw `completed` field to decide whether cleanup may reap an origin. A journal schema or version change therefore must preserve or update both readers; missing, malformed, or incomplete journal state must continue to fail closed by retaining the origin. Prefer replacing this coupling with a public `follow_up` predicate that encapsulates journal parsing and completion semantics.

Related: [[post-land-effects-are-idempotent-and-land-safe]], [[needs-user-not-terminal-for-cleanup]]
