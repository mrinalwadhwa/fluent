---
name: dashboard-layout-contract-is-acyclic
description: Keep dashboard interaction state and rendering acyclic by placing shared geometry and line projection in the layout contract
metadata:
  type: architecture
---

The dashboard separates immutable Work-status projection (`dashboard/snapshot.rs`),
interaction state (`dashboard/app.rs`), layout calculations (`dashboard/layout.rs`),
and Ratatui drawing (`dashboard/render.rs`). Preserve that dependency direction:
`app` and `render` may both consume `layout`, but neither may depend on the other.

When a new control needs scroll limits, viewport dimensions, or a mapping from a
selected item to rendered lines, extend the input-oriented layout contract rather
than importing rendering code into the state machine. The contract must calculate
bounds from the actual terminal geometry and projected content, so state can clamp
offsets and retain selection visibility without assuming that rows never wrap.
This keeps transitions deterministic and lets rendering remain a pure consumer of
state. Related: [[dashboard-rendered-overflow-tests]].
