---
name: terminal-flood-fixtures-own-output
description: Gate fixture-owned tmux output around confirmed read-only Observe and concurrent dashboard navigation
metadata:
  type: testing
---

Live dashboard flood tests must keep Observe attached and read-only while output
continues during navigation and refresh. Do not start a producer by injecting
`send-keys` through an inferred tmux client: tmux 3.8 can reject that command
from Observe even with an explicit pane target.

Prepare the producer in the fixture's explicitly targeted pane before attaching
Observe, but hold output behind a start gate. Release that gate only after tmux
confirms the read-only client. Keep a separate finish gate closed throughout
navigation, then verify completion, reconnect identity, and bounded rendering.
This preserves the live overlap without granting the observer Control.

Bound both gates and stop the producer if its fixture directory disappears.
Exercise missing start, missing finish, removed-directory, and successful paths,
and retain kill/reap cleanup when assertions fail. A producer that finishes
before Observe attaches cannot prove dashboard responsiveness during a flood.

Related: [[terminal-host-tests-exercise-live-boundaries]],
[[terminal-control-lease-follows-client]].
