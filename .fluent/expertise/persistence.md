# Persistence

Fluent persists operations, journals, Work Items, and scheduler state under
`.fluent/work/`. Treat both their serialized fields and their deterministic
identifiers as durable formats.

## Patterns

- [reconcile-deterministic-identity-changes](persistence/patterns/reconcile-deterministic-identity-changes.md) — read when changing how persisted operation or effect ids are derived
