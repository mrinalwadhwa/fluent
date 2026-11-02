---
name: dashboard-responsiveness-tests-gate-real-reads
description: Hold a confirmed snapshot read open while testing dashboard navigation, rendered views, and exit through a PTY
metadata:
  type: testing
---

To prove that dashboard refresh cannot stall interaction, keep the reader
blocked until the interaction assertions finish. Channel gates in
`src/dashboard/tests.rs` exercise production key handling, rendering, and
completion separately. A passing state transition after the reader has already
finished does not establish responsiveness during a read.

The public journey in `tests/fixtures/dashboard-journey.py` gates the real Work
loader with a FIFO created in the fixture's Work Item collection after the
initial screen appears. Successfully opening its writer with `O_NONBLOCK`
confirms that the reader reached the FIFO. Retain the writer without sending
bytes or EOF while driving selection, view changes, and quit through the PTY.
Assert rendered results and successful process exit before releasing the gate;
this also detects an accidental blocking join during dashboard cleanup.

Keep waits bounded, reap the dashboard on failure, remove the FIFO before
comparing stored Work bytes, and retain the existing no-mutation assertion.
The journey runs through
`tests/behaviors/operations/test-work-status-dashboard.sh`.

Related: [[dashboard-local-refresh-owns-no-ui-state]],
[[dashboard-rendered-overflow-tests]],
[[terminal-host-tests-exercise-live-boundaries]].

`initial_local_read_does_not_block_first_draw` also drives the production event
loop through `DashboardSession` with a Ratatui test backend. It waits until a
FIFO in the Work collection confirms the initial loader is blocked, then
asserts rendered view changes, help, and quit before releasing EOF. Keep the
initial draw and local read request inside that tested coordinator.
