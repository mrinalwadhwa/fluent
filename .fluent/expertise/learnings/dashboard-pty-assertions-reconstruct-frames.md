---
name: dashboard-pty-assertions-reconstruct-frames
description: Reconstruct dashboard PTY frames and wait for rendered readiness before sending keys or asserting Work content
metadata:
  type: testing
---

Ratatui draws changed cells on later frames. Stripping ANSI escapes from a
dashboard PTY capture and concatenating the remaining bytes does not reconstruct
the text an operator saw: unchanged cells may never appear again beside the
updated text.

Reuse the `Screen` projection in `tests/fixtures/dashboard-journey.py` to replay
cursor movement and text updates at the capture's terminal dimensions. Normalize
whitespace only after reconstructing the screen. When assertions inspect both
the Work list and a later errors overlay, retain successive completed screens;
the final screen alone cannot establish what the earlier view displayed.

The shell dashboard behavior harness uses Ratatui's cursor-hide sequence as its
completed-frame boundary. Keep that boundary aligned with the backend when
changing how the harness captures output.

The dashboard renders a loading frame before its asynchronous initial Work read
finishes. Before sending Work-dependent keys or quitting a capture, replay the
recorded output until the reconstructed screen shows the expected dashboard
controls and no longer shows the local loading message. Use a bounded deadline,
enable recorder flushing (`script -qF` in the shell harness), and propagate
readiness failure through the pipefail-enabled capture pipeline. A fixed sleep
can capture only the loading screen when the full suite slows the first read.
Keep a regression that delays the real initial loader with a FIFO beyond the
former capture delay, then verifies that the completed capture contains Work.

Related: [[dashboard-rendered-overflow-tests]],
[[dashboard-responsiveness-tests-gate-real-reads]].
