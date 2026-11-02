---
name: terminal-focus-requires-visible-identity
description: Separate terminal visibility, input focus, and retained Control when routing input and projecting dashboard controls
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

Inspecting worker details may hide the terminal while retaining its attachment,
invocation, and Control lease. Keep detail visibility, the pane receiving keys,
and retained Control as separate state. `terminal_view::Display` reports focus
and retained Control for presentation; these flags never grant input authority.
Runtime attachment methods still validate the live lease and exact invocation.

Help and the footer share the model's control projection. Advertise literal
terminal input only when the attachment retains Control, including when
navigation owns focus; an Observe attachment alone cannot send it. While native
input owns focus, advertise its return-to-navigation chord. Fit whole controls
within the footer rather than clipping commands, with quit and return controls
available at the minimum supported dimensions.

Verify authority-dependent hints with real Observe and Control attachments
through dashboard key dispatch. Check both the rendered hint and actual byte
delivery, unchanged invocation identity, and hint removal after releasing
Control. An assertion on an App effect alone cannot prove the attachment will
accept the advertised action. After Control reconnects a display client, wait
for a new paint rather than accepting its retained Observe frame as readiness.

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
