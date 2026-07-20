# Work Model

The Work model (`src/work_model.rs`) holds the core data structures — WorkItem,
Attempt, Task, MergeCandidate — and their JSON-file storage. WorkItems are
stored split: top-level fields serialize through `WorkItemRecord`, while
attempts, tasks, and merge candidates persist as separate records.

## Patterns

- [extend-work-item-backward-compatibly](work-model/patterns/extend-work-item-backward-compatibly.md) — read when adding a field to WorkItem or another split-stored model type
