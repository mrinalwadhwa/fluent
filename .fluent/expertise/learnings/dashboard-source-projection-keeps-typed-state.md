---
name: dashboard-source-projection-keeps-typed-state
description: Keep dashboard source loading and worker lookup typed; format operator detail lines in a separate presentation boundary
metadata:
  type: architecture
---

The project dashboard loads each checkout into a bounded, read-only source
snapshot. Keep that projection responsible for source identity, Work and Task
state, terminal descriptors, diagnostics, and typed lookup. Build display labels
and detail lines in a presentation layer shared by layout and rendering, so a
change to terminal wording or pane text does not change the source contract.

Layout may consume the same presentation values to calculate scroll bounds, but
it must receive state as inputs rather than importing `App`. Related:
[[dashboard-layout-contract-is-acyclic]].
