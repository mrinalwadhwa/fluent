---
name: native-relay-tests-drain-before-comparing
description: Keep large-payload fixture work outside the Codex relay's bounded display drain while proving evidence settles before display consumption
metadata:
  type: testing
---

The native Codex WebSocket relay bounds how long it drains queued display
frames after provider completion. A test can exhaust that interval by building
or comparing large catalog payloads while the display remains unread. The
resulting disconnect can reflect fixture CPU work even when delivery preserves
the intended response sizes and order.

Construct large response strings before opening the fixture sockets. After
provider completion, snapshot the canonical transcript and relay trace before
the display reads any frame; those snapshots prove evidence settled independently
of display consumption. Then drain the expected wire frames into memory before
parsing JSON or comparing large payloads. Assert complete payloads, FIFO order,
and turn completion from the appropriate snapshots and received frames.

Keep the production drain policy and response sizes intact. Reading evidence
only after draining would lose the independence check, while interleaving costly
comparisons with reads would make fixture processing consume the drain budget.
The production relay journey in `tests/native_agent_tasks.rs` demonstrates this
ordering.

Related: [[native-codex-display-is-disposable]],
[[prove-settlement-path-not-just-outcome]].
