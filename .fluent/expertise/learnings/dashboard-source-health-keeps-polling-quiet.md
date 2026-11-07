---
name: dashboard-source-health-keeps-polling-quiet
description: Keep routine polls quiet while reporting source health independently of filtered Work rows
metadata:
  type: convention
---

The project dashboard separates read-health bookkeeping from visible progress.
Healthy source headers keep their project and host identity stable during
routine polls: successful receipt times and source hashes do not appear on
every refresh. Add a source disambiguator when project names collide on the
same host. Show initial loading and operator-requested checking once in the
global health row above the shortcuts, including a manual request coalesced
behind an in-flight read.

Omit source groups with no Work in the selected view from drawing and list
scroll bounds, while retaining their health and diagnostics. Mark visible
affected groups compactly and count failed and overdue sources in the global
health row even when their groups are hidden. Keep full diagnostics and last
successful read times in Read issues rather than expanding source headers.
Initial and requested reads use a delayed, fixed-width global indicator that
can coexist with warnings; routine polls remain quiet. Preserve the last good
Work rows and keep read timestamps separate from recorded Work update times.

Pending reads can become stale without completing. Poll ticks must still
request a redraw while the reader is blocked so the time-based warning becomes
visible; suppressing routine progress must not suppress health reevaluation.

The presentation and refresh boundaries live in `src/dashboard/render.rs` and
`src/dashboard.rs`; `documentation/behaviors.md` defines the source-header
contract. Related: [[dashboard-local-refresh-owns-no-ui-state]],
[[dashboard-collection-failures-preserve-stale-snapshot]],
[[dashboard-display-metadata-preserves-ssh-schema]].
