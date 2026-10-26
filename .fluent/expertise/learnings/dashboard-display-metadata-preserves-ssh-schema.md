---
name: dashboard-display-metadata-preserves-ssh-schema
description: Keep local dashboard display metadata out of strict SSH snapshots until an explicit protocol compatibility change
metadata:
  type: architecture
---

The dashboard's local projection and SSH snapshot do not carry identical fields.
The SSH snapshot embeds the serializable worker type, and both the snapshot and
worker types reject unknown fields. Adding an optional serialized field can
therefore break older peers even when the new reader accepts its absence.

Keep local display metadata outside the wire representation unless an explicit
protocol compatibility change admits it. Update times illustrate this boundary:
local rows derive them from published lifecycle transitions or recorded Task
times, worker serialization skips them, and remote rows retain an unavailable
value. Do not substitute the polling time for a missing recorded update time.
Remote timestamp support requires a compatibility plan for both readers and
writers, rather than simply enabling serialization on the shared worker type.

The boundary lives in `src/dashboard/project.rs` and
`src/dashboard_sources/snapshot.rs`. Related:
[[dashboard-source-projection-keeps-typed-state]],
[[model-owned-lifecycle-transition-ledger]], [[backward-compatible-serde-fields]].
