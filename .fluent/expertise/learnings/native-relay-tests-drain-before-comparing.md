---
name: native-relay-tests-drain-before-comparing
description: Keep expensive Codex fixture validation outside live protocol deadlines and the bounded display drain
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

The required-frame backpressure fixture also has a bounded provider input wait.
After receiving its large response, send the next protocol request before
parsing or validating that payload, then drain traffic and settle the peers.
Validate the retained response afterward. Even validation after a successful
read can consume the provider's deadline while it waits for the next request.
The slow-validator regression in `tests/native_agent_tasks.rs` deliberately
outlasts that wait to verify this ordering without relaxing transport deadlines
or payload and duplicate-frame assertions.

Related: [[native-codex-display-is-disposable]],
[[prove-settlement-path-not-just-outcome]].
