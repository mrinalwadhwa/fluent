---
name: terminal-control-lease-follows-client
description: Hold a managed terminal's Control lease in the writable tmux client for its full attachment lifetime
metadata:
  type: architecture
---

Managed terminal attachment observes by default. A Control attachment may
send input only while it holds the exclusive process lease. The writable tmux
client itself must inherit that lease descriptor, so the lease remains held if
the Fluent CLI exits while the client is attached. A waiting shell wrapper is
insufficient: it may die while its writable child remains connected and allow a
second controller. Releasing or detaching the client releases Control without
canceling the worker.

Related: [[managed-terminal-receipt-precedes-publication]].
