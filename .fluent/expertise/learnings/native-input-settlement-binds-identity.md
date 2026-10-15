---
name: native-input-settlement-binds-identity
description: Settle native Tasks only for the current execution generation, assigned conversation, and latest admitted input
metadata:
  type: architecture
---

Native agent input and Task settlement share a host-owned admission boundary.
The owner assigns an execution generation to each invocation and checks the
provider's conversation and turn or session identity before treating events as
Task evidence. Each admitted input receives a sequence number. The owner
serializes input admission with settlement and accepts completion only after it
accounts for the latest admitted sequence. An older turn, another conversation,
or an earlier generation cannot finish the current Task.

Writer continuations may reuse the assigned provider conversation, but they
receive a new execution generation and terminal. Review, Learner, and rebase
Tasks use fresh conversations. Provider notifications alone do not determine
which Work Task has completed.

Related: [[native-pi-input-failures-return-handled]],
[[terminal-control-lease-follows-client]].
