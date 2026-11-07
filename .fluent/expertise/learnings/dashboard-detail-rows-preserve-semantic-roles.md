---
name: dashboard-detail-rows-preserve-semantic-roles
description: Carry section and selected-worker roles through shared detail wrapping instead of inferring structure from display text
metadata:
  type: architecture
---

The dashboard's detail projection carries a semantic role alongside each display
string. Build overview sections explicitly and preserve those roles through
wrapping. Rendering uses the roles to style headings and selected workers;
navigation uses the same wrapped rows to locate the selected worker.

Work titles and guidance can resemble section headings or selection markers.
Searching their text for a heading prefix or a leading selection marker can
produce invalid section ranges or scroll to an unrelated row. Display text must
not determine section boundaries or worker selection.

Keep these row roles in the shared layout contract consumed by the app and
renderer. Regressions should render section-like titles with enough workers to
overflow the minimum viewport, navigate to a later worker, and verify both its
visible selection and the exact worker page opened.

Related: [[dashboard-layout-contract-is-acyclic]],
[[dashboard-source-projection-keeps-typed-state]],
[[dashboard-rendered-overflow-tests]].
