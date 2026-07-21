# Serialize aggregate Work writes

## Context

Use this pattern when code reads a `WorkItem`, mutates any top-level field or
child Attempt, Task, or Merge Candidate, and calls `write_work_item`. The store
persists those children as separate records and prunes records absent from the
aggregate, so a stale snapshot can otherwise revert or delete concurrent work.

## Mechanism

`WorkModelStore` attaches the top-level record's persisted storage revision to
every assembled `WorkItem`. It takes the per-item `model.lock` before an
aggregate write, compares the caller's observed revision with the current
record, and increments the revision before replacing any top-level or split
record. A mismatch returns `StaleWorkItem` before pruning. Reload the Work Item,
reapply the intended field-level change, and retry through the same boundary.

New in-memory Work has an unobserved revision. Its first write may create a
missing record, but it cannot overwrite a record another creator installed
first. The store updates the in-memory revision after a successful write, so a
single owner can perform sequential mutations without reloading.

## Example

A Learner retry and land may both read the same candidate. If land writes
`Merged` first, its write advances the storage revision. The retry's aggregate
write then returns `StaleWorkItem` before it can replace the candidate record
with its old `Ready` snapshot. The retry reloads and observes that it must use
the post-land handoff-only path.
