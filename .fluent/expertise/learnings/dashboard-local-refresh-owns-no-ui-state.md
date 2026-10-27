---
name: dashboard-local-refresh-owns-no-ui-state
description: Keep local snapshot workers read-only, merge completion into current UI and SSH state, and detach unfinished reads on exit
metadata:
  type: architecture
---

After the initial synchronous load, the project dashboard runs local discovery
and Work reads in a background worker that owns copies of its inputs. Keep at
most one local read in flight: periodic ticks do not queue more reads, while
manual requests during a read coalesce into one follow-up. The worker returns
local snapshots and diagnostics; the event loop owns interaction state.

Apply a completed local result to the current app state. Retain the SSH
snapshots and diagnostics present at completion, since remote refreshes may
have finished while the local read was pending. Reconcile the current filter
and composite selection identity through the app's refresh path rather than
restoring interaction state captured when the read started.

Join a local reader only after it finishes. Dashboard exit deliberately drops
an unfinished reader's handle without joining it, so the read may continue
until it completes or the process exits. This is safe because the reader owns
its inputs and only observes Work; it owns no terminal or durable execution
effects. Do not copy the SSH collection's join-on-drop behavior into this local
reader. This exception does not relax ownership of scheduler workers.

The boundary lives in `src/dashboard.rs`; the SSH collection's distinct cleanup
lives in `src/dashboard_sources/collection.rs`. Related:
[[dashboard-source-projection-keeps-typed-state]],
[[dashboard-collection-failures-preserve-stale-snapshot]],
[[scheduler-workers-retain-join-ownership]],
[[dashboard-responsiveness-tests-gate-real-reads]].
