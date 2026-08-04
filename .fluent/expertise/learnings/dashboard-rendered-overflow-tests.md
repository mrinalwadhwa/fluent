---
name: dashboard-rendered-overflow-tests
description: Dashboard overflow and responsive-layout tests must render constrained TestBackend frames and assert visible content and pane bounds
metadata:
  type: testing
---

For dashboard behaviors that promise navigation, truncation, wrapping, or
responsive panes, test the rendered terminal frame with Ratatui's `TestBackend`.
An assertion only on private selection or scroll counters does not establish that
the operator can see the selected row, diagnostic, or later detail line.

Build fixtures large enough to overflow the actual constrained region and assert
that navigation changes the buffer to reveal the target content. At layout
breakpoints, also assert the intended panes or divider remain intact, and include
long ASCII plus display-width-sensitive text when checking compact-row containment.
This makes the test cover the layout contract and renderer together while state
transition tests retain their narrower role. Related:
[[dashboard-layout-contract-is-acyclic]].
