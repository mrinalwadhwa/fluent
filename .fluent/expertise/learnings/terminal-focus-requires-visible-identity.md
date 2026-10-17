---
name: terminal-focus-requires-visible-identity
description: Grant embedded input focus only when the dashboard shows the selected invocation identity and Control state
metadata:
  type: architecture
---

The dashboard's Control lease and native input focus are separate. Navigation
may retain Control, but native input requires a visible terminal with its Task,
execution generation, and Control state. A focus action from the narrow list
must reveal the terminal pane. If the minimum-size screen or another view hides
the terminal, release focus and reject native key, paste, and mouse routing.
Enlarging the window does not restore focus automatically; the operator must
explicitly focus the retained invocation again.

Use realistic Work Item IDs and full generation lengths when checking visible
identity. Short labels can pass while production identities lose their Task
and generation to clipping, including in a wide dashboard's split pane. The
identity's wrapped header consumes terminal rows: drawing, worker viewport
sizing, cursor placement, and mouse translation must use the same content area.

[`src/dashboard/tests.rs`](../../../src/dashboard/tests.rs) covers rendered
identity and geometry at supported widths and rejects hidden input. The live
selected-worker journey in
[`tests/dashboard_terminal.rs`](../../../tests/dashboard_terminal.rs) also
checks resize, explicit focus restoration, and exact received bytes.

Related: [[dashboard-rendered-overflow-tests]],
[[dashboard-layout-contract-is-acyclic]], [[terminal-control-lease-follows-client]].
