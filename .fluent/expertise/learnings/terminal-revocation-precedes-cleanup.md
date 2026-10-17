---
name: terminal-revocation-precedes-cleanup
description: Revoke attachment access before terminal cleanup waits for Control, retaining execution authority until settlement is verified
metadata:
  type: architecture
---

Managed terminal settlement marks a matching published descriptor `Settling`
and disables observation and Control admission for that execution generation
before cleaning descendants, closing the server, or waiting for the Control lease.
Existing clients must observe that revocation and settle themselves so they
can release Control. Full-screen clients revalidate liveness even when terminal
dimensions have not changed; checking only during resize can leave cleanup
waiting on an attached client.

Revoking attachment access does not prove execution cleanup completed. An
embedded view can become Ended and reject input while the execution record and
host-owned resource receipt still retain unresolved cleanup authority. Preserve
those records after a failed cleanup and let retry verify settlement before
retiring the receipt. A returned dashboard or an exited tmux server alone cannot
establish successful cancellation.

The cancellation regressions in
[`src/terminal_sessions.rs`](../../../src/terminal_sessions.rs) cover revocation
without resize, failed cleanup with retained authority, and nested full-screen
cancellation followed by retry. Keep those observations ahead of fixture
fallback teardown so cleanup cannot manufacture the claimed success.

Related: [[terminal-control-lease-follows-client]],
[[managed-terminal-receipt-precedes-publication]],
[[prove-settlement-path-not-just-outcome]].
