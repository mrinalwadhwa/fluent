# Reconcile deterministic identity changes

## Context

Use this pattern when code changes the algorithm that derives a path or id for
durable state. The serialized schema may stay unchanged while the new lookup
algorithm makes an older record invisible.

## Mechanism

Derive both the current and legacy identities from the immutable origin. Probe
for complete records at both locations, parse and validate any record against
its expected identity and origin, and fail closed when both locations contain
state. When only legacy state exists, keep its operation id and derive or
validate every downstream effect id with the legacy algorithm. Apply the same
discovery boundary to recording, replay, recovery, and cleanup.

## Example

Post-land V1 operations originally used
`<work-item-id>-<merge-candidate-id>` and filename-normalized follow-up ids.
Current operations use collision-safe hashes. `follow_up` discovers either
layout from the landed origin and replays an old operation with its old
Observation and derived Work ids, so an upgrade cannot duplicate effects or
hide completion evidence from cleanup.
