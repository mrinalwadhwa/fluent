---
name: native-codex-display-is-disposable
description: Keep Codex TUI display backpressure separate from native evidence capture and Task supervision
metadata:
  type: architecture
---

The native Codex app-server relay records accepted provider notifications as
canonical Task evidence before sending them to the TUI. Display delivery uses
a bounded spool. If the TUI stalls or its spool fills, detach that display
connection while the provider reader continues to record events and supervise
completion. A slow terminal or redirected stderr must never sit on the relay's
input admission or evidence path.

Test this property through the production WebSocket relay; the raw Unix JSON
relay used by some fixtures has different transport behavior.

Related: [[native-input-settlement-binds-identity]],
[[external-adapter-contract-tests-drive-real-protocol]].
