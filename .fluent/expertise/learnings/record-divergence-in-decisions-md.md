---
name: record-divergence-in-decisions-md
description: Deliberate divergences from approach.md belong in decisions.md (durable), not just progress.md (round-scoped)
metadata:
  type: convention
---

When an implementation deliberately diverges from an interface or shape specified in `approach.md` — for example collapsing `record_post_land_operation(origin, merged_commit, batch_ref, disposition)` into `record_post_land_operation(project_root, batch, review_request)` so the normalized batch is the single source of truth — record the divergence in `decisions.md`, not only in the round's `progress.md`.

`decisions.md` is the durable record a future contributor consults; `progress.md` notes are round-scoped and stop being read once the round closes. A divergence recorded only in progress notes later reads as accidental drift from the approach. The architecture reviewer flags an unrecorded, otherwise-reasonable divergence as a minor finding for exactly this reason.

Related: [[keep-architecture-doc-in-sync]]
