---
name: dashboard-pty-assertions-reconstruct-frames
description: Replay Ratatui cursor updates into a screen before asserting visible dashboard text in PTY captures
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

Related: [[dashboard-rendered-overflow-tests]],
[[dashboard-responsiveness-tests-gate-real-reads]].
