---
name: follow-up-journal-schema-boundary
description: Keep post-land journal interpretation in follow_up so cleanup does not become a second schema owner
metadata:
  type: architecture
---

`src/follow_up.rs` owns the post-land operation and journal formats. Other subsystems should ask that module for lifecycle facts instead of parsing `journal.json` themselves.

Cleanup calls `follow_up::post_land_operation_complete` rather than parsing the journal. That boundary validates operation and batch identities and digests, journal receipts, and the durable Observation, Work, and queue effects before cleanup may reap an origin. Keep missing, malformed, conflicting, or incomplete evidence fail-closed by returning anything other than `Ok(true)`.

Related: [[post-land-effects-are-idempotent-and-land-safe]], [[needs-user-not-terminal-for-cleanup]]
