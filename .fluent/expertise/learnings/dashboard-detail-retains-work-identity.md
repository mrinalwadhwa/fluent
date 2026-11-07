---
name: dashboard-detail-retains-work-identity
description: Keep open dashboard details and action feedback bound to their source and Work Item independently of filtered list selection
metadata:
  type: architecture
---

The project dashboard reconciles selection with the counted view while the Work
list is open. An overview or worker page instead resolves its exact source and
Work Item across refreshed snapshots, even when the item moves into another
category. Keep the original list filter and reconcile to a visible row on Back.
If the exact Work key disappears, clear the detail selection rather than opening
a nearby item or an identically named item from another source.

Action menus retain the offered identity independently of current selection.
Bind every result to that offered or prepared action, including preparation
rejection, declined confirmation, clipboard success or failure, and command
completion. Selection may already be absent when feedback arrives, so it cannot
supply the result's identity. Keep the original Work Item and source context in
the full report. Feedback without a bound identity is not displayable; bound
feedback may remain readable after disappearance but must be hidden when a
different source or Work Item is selected.

Verify category transitions on both overview and worker pages. Exercise
disappearance before preparation, cancellation, and copying, then open both
another Work Item and the same Work Item ID from another source. Check the
rendered result and complete detail report so neither the pinned summary nor
Last action can misattribute feedback. The reconciliation and feedback
boundaries live in `src/dashboard/app.rs`, `src/dashboard/model.rs`, and the
action coordinator in `src/dashboard.rs`.

Related: [[dashboard-refresh-preserves-reading-position]],
[[dashboard-local-refresh-owns-no-ui-state]],
[[terminal-focus-requires-visible-identity]].
