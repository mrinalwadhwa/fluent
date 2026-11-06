---
name: dashboard-source-health-keeps-polling-quiet
description: Keep healthy source headers stable during polling and show read activity only for explicit checks or source warnings
metadata:
  type: convention
---

The project dashboard separates read-health bookkeeping from visible progress.
Healthy source headers keep their project and host identity stable during
routine polls: successful receipt times and source hashes do not appear on
every refresh. Add a source disambiguator when project names collide on the
same host. Show initial loading and operator-requested checking once in the
global header, including a manual request coalesced behind an in-flight read.

Keep failures and stale retained snapshots visible at the affected source.
Warnings may expose the last successful read time, and the errors view retains
full diagnostics. Preserve the last good Work rows and keep read timestamps
separate from recorded Work update times.

Pending reads can become stale without completing. Poll ticks must still
request a redraw while the reader is blocked so the time-based warning becomes
visible; suppressing routine progress must not suppress health reevaluation.

The presentation and refresh boundaries live in `src/dashboard/render.rs` and
`src/dashboard.rs`; `documentation/behaviors.md` defines the source-header
contract. Related: [[dashboard-local-refresh-owns-no-ui-state]],
[[dashboard-collection-failures-preserve-stale-snapshot]],
[[dashboard-display-metadata-preserves-ssh-schema]].
