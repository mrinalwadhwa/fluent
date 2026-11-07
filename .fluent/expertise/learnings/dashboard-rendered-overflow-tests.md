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

Rendered diagnostic words can span terminal rows, and source paths can push
the rest of a diagnostic onto another page. Collect only the rendered error
body while paging to the scroll limit, then normalize display whitespace before
checking diagnostic content. Check the complete source error and exact last
successful read timestamp as well as the warning marker. Make the fixture place
the marker across a row boundary regardless of the host's temporary-directory
path; a raw substring check on one frame otherwise depends on that path's
length. Keep separate assertions for preserved Work rows and unchanged Work
update times so normalization does not weaken the stale-snapshot contract.

Dashboard action labels can change while their CLI commands remain exact. In
command-wrapping regressions, require a nonempty selection of the expected
command rows and prove the fixture actually wraps at the constrained width
before reconstructing each complete command. Keep selectors aligned with the
current action labels or iterate over explicit expected commands. Otherwise a
label rename can leave the preservation assertion inside an empty loop, even
when separate assertions still find the commands in the unwrapped details.
