---
name: legacy-evidence-compatibility-stays-isolated
description: Recover historical evidence through a separate exact compatibility classifier that cannot weaken current execution rules
metadata:
  type: architecture
---

When Fluent must recover persisted evidence that predates the current evidence
contract, keep the compatibility classifier separate from the classifier used by
new Task execution. Call it only from the narrow historical recovery route. A
legacy artifact that proves a historical pause must never become sufficient
evidence for a newly produced Task, because doing so silently weakens the
current no-progress proof in [[provider-outage-evidence-fails-closed]].

Treat the preserved historical structure as a closed grammar. Normalize only
fields known to vary without changing meaning, such as paths, identifiers, and
timing metadata; require those fields to exist with the expected type and
internal consistency before removing them. Compare the entire remaining event
structure to a checked-in manifest. Reject missing, extra, malformed, reordered,
or unknown events and fields, plus any changed terminal semantics, model usage,
tool activity, permission activity, cost, or token evidence.

Apply the migration only after every blocking record independently satisfies
the legacy grammar and all other recovery preconditions. Perform the resulting
state change as one lock-held Work-model mutation, and leave the persisted state
unchanged when any record is inconclusive. Route-level tests must prove both
halves of the boundary: exact preserved artifacts resume through the public
recovery path, while structural deviations remain blocked and the same legacy
artifact does not classify a current Task.
