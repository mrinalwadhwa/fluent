---
name: recovery-cursors-advance-after-durable-ingest
description: Advance recovery cursors after durable processing or conclusive rejection while retaining retryable failures
metadata:
  type: architecture
---

A reconnect loop cannot recover reliably by requesting one fixed-size history
page. Persist a per-stream high-water mark, pass it to the external history
API, follow every pagination cursor, sort recovered events in source order, and
route them through the same durable inbox as live ingress.

Advance the high-water mark only after durable processing or conclusive
rejection. If processing fails, retain the last safe cursor so the next
reconnect includes the event again. Live delivery and recovered history may
overlap; the inbox's stable event identity must deduplicate that overlap rather
than relying on the cursor alone.

Distinguish conclusive authorization rejection from processing failure at both
ingress and action authorization. A configured participant can pass ingress
while lacking permission to request Work. Treating that denial as retryable
traps history at the same event and withholds later authorized replies. Consume
a conclusive rejection without granting provider or Work access. For an admitted
message rejected at action authorization, persist its rejected identity separately
from accepted history so redelivery after a later authority grant cannot execute
the old request. Require a new input after the authority change.

Keep provider failures, unavailable Work state, and incomplete accepted approval
receipts pending for reconciliation; a rejection-shaped error must not discard
an accepted effect whose completion remains uncertain. Preserve the distinction
in the live inbox as well as history recovery.

Tests need more than one page, a failure between two recovered events, and a
live/history overlap. Assert that restart begins from the last safe cursor and
that every accepted contribution has one visible effect. Include an unconfigured
actor and a participant without Work approval authority before an authorized
reply, then change authority and redeliver the rejected Work request. Related:
[[durable-external-effects-use-reconcilable-identities]],
[[approval-records-bind-current-authority]],
[[collaboration-inbox-isolates-thread-failures]].
