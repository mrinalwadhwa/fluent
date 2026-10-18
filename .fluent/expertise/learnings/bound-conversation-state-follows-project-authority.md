---
name: bound-conversation-state-follows-project-authority
description: Keep bound conversation state under project authority and require its snapshot through ingest, recovery, and failure reporting
metadata:
  type: architecture
---

The local Slack runner has two persistence authorities. Before a thread binds,
the runner-owner root may retain the full ingress needed to offer and select a
project. After binding, contribution text, provider sessions, pending turns,
planning state, and other active conversation data belong under the bound
checkout's ignored `.fluent/work/slack/` runtime tree. The owner root retains
only the private project mapping, a redacted routing envelope, and an opaque
deduplication identity.

Do not treat a stripped owner-side index record as a fallback snapshot. If the
authoritative bound snapshot is missing or unreadable, fail closed instead of
loading incomplete state and replaying the turn as unbound work.

Publishing the project snapshot and owner-side index spans two filesystem
locations, so atomic replacement of each file is insufficient. Write a durable
commit record that contains the previous index state, next index state, and
snapshot digest before changing either representation. Recovery promotes the
next index only when the project snapshot matches the recorded digest;
otherwise it restores the previous index. Exercise crashes after either write
order.

The missing-snapshot rule also applies to Discord's separate project runtime.
Require the authoritative snapshot in every path that writes conversation state,
including inbox failure reporting. A defaulting reader in an error handler can
recreate an empty conversation after ingest correctly refused it, allowing the
next retry to repeat accepted provider turns. Status inspection may report
restoration guidance without manufacturing conversation state. Only an explicit
unfinished initialization may create an initial empty snapshot; an established
routing binding is not initialization authority.

Test this through the production inbox processor, not only direct ingest and
recovery helpers. Remove an established snapshot, enqueue an accepted input's
redelivery, and repeat processing. Assert that the file remains absent and the
input remains pending, with no new provider call or delivery. Restore the
original bytes and verify that the duplicate settles without replaying effects.

Related: [[atomic-write-replace-through-utility]],
[[registry-read-modify-write-needs-flock]],
[[collaboration-inbox-isolates-thread-failures]].
