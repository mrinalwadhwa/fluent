---
name: managed-terminal-receipt-precedes-publication
description: Keep a host-owned terminal resource receipt authoritative from setup through verified descendant settlement, including before descriptor publication
metadata:
  type: architecture
---

Managed terminal setup reserves a host-owned resource receipt before launching
the server and publishes the attachable descriptor only after the server and
session start. Cancellation must use the receipt even if setup fails before
publication or the server exits first: neither a missing descriptor nor an
exited server proves that pane descendants are gone. Retire the receipt and
private socket namespace only after the owned generation has settled.

The execution cancellation path also serves ordinary headless workers and
nested execution records. Check for a terminal receipt before applying the
terminal-specific Task path and generation rules, so those executions remain
cancelable without a terminal resource.

Related: [[terminal-session-cleanup-covers-partial-setup]],
[[process-inventory-distinguishes-exit-from-unavailable]].
