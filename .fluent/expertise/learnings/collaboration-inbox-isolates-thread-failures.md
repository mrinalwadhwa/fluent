---
name: collaboration-inbox-isolates-thread-failures
description: Contain thread-local processing and failure-reporting errors while independent conversation inputs continue
metadata:
  type: architecture
---

The Discord runner shares an owner-wide inbox across conversations and projects.
Contain an individual input's processing or failure-reporting error within that
entry, retain it for recovery, and continue visiting independent inputs before
returning the batch failure. A strict snapshot reader must remain strict even
when the error reporter cannot load the snapshot.

Exercise both history recovery and the production inbox processor. History
isolation alone does not keep new command interactions or approval clicks moving,
because those inputs cannot be reconstructed from ordinary message history.
Put a damaged thread's older input first, then queue a new command and a valid
approval for healthy conversations. Repeat the batch and redeliver the healthy
inputs: their effects must occur once while the damaged input stays queued and
its missing snapshot stays absent. Restore that snapshot and verify the retained
duplicate settles without repeating its provider turn or delivery.

Related: [[bound-conversation-state-follows-project-authority]],
[[recovery-cursors-advance-after-durable-ingest]],
[[scheduler-service-contains-project-failures]].
