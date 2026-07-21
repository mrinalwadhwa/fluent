# Serialize aggregate Work writes

## Context

Use this pattern when code reads a `WorkItem`, mutates any top-level field or
child Attempt, Task, or Merge Candidate, and calls `write_work_item`. The store
persists those children as separate records and prunes records absent from the
aggregate, so a stale snapshot can otherwise revert or delete concurrent work.

## Mechanism

`WorkModelStore` attaches the top-level record's persisted storage revision to
every assembled `WorkItem`. Aggregate readers and writers take the per-item
`model.lock`. A writer compares the caller's observed revision with the current
record, rejects revision exhaustion, and durably records the complete target
snapshot under `.fluent/work/transactions/` before replacing any split record.
It writes and prunes children first, publishes the incremented top-level record
last, then removes the transaction. A mismatch returns `StaleWorkItem` before
pruning. Reload the Work Item, reapply the intended field-level change, and
retry through the same boundary.

If a child write, process failure, or crash interrupts publication, readers and
later writers finish the durable transaction while holding the same model lock
before they assemble or mutate the Work Item. Listing Work Items first recovers
transactions too, including a creation whose top-level record was not yet
published. Code that already holds `model.lock` must call
`read_work_item_under_model_lock` to avoid reacquiring the lock.

New in-memory Work has an unobserved revision. Its first write may create a
missing record, but it cannot overwrite a record another creator installed
first. The top-level record stays absent until all split children are durable.
The store updates the in-memory revision after a successful write, so a single
owner can perform sequential mutations without reloading.

## Example

A Learner retry and land may both read the same candidate. If land writes
`Merged` first, its write advances the storage revision. The retry's aggregate
write then returns `StaleWorkItem` before it can replace the candidate record
with its old `Ready` snapshot. The retry reloads and observes that it must use
the post-land handoff-only path.
