---
name: dashboard-refresh-preserves-reading-position
description: Preserve deliberate overview scrolling during refresh while keeping worker navigation and action feedback visible
metadata:
  type: convention
---

The Work Item overview retains worker selection independently of the operator's
reading position. Before replacing a snapshot, record whether the selected
worker is visible. After refresh, reveal its new position if it was previously
visible; otherwise preserve deliberate scrolling into guidance or identifiers,
subject to the new scroll bounds. An elapsed-time update must not pull the view
back to the worker table. Explicit worker navigation and return to the overview
still reveal the retained selection.

Keep the action-result summary outside the scrollable detail body so revealing
a selected worker cannot hide success, failure, interruption, or validation
feedback. Retain the complete message in scrollable details. The shared layout
must reserve the feedback row consistently for drawing, scroll bounds, and
selected-worker positioning.

Verify these policies together in rendered frames at minimum and wide sizes:
return with a late worker selected, reorder a visible selection, refresh elapsed
time while reading secondary context, and return from an action with both its
outcome and the selected worker visible.

Related: [[dashboard-detail-rows-preserve-semantic-roles]],
[[dashboard-local-refresh-owns-no-ui-state]],
[[dashboard-rendered-overflow-tests]].
