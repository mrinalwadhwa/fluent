2026-06-09 — Work model storage still had a compatibility bridge where
`.factory/work/items/<id>.json` could contain nested Attempts, Tasks, and
Merge Candidates when no split records existed. The adoption plan called
for live Work objects to move into separate durable collections instead
of carrying one nested Work Item JSON file indefinitely.
→ Resolved: `4f9c52f` and `bc2c4e6` made split Work storage
authoritative. `WorkModelStore` now parses item files as Work Item
metadata, assembles Attempts, Tasks, and Merge Candidates from
`.factory/work/attempts/`, `.factory/work/tasks/`, and
`.factory/work/merge-candidates/`, ignores nested operational collections
in item JSON, updates storage documentation and behavior contracts, and
adds focused storage, CLI, behavior, and external-review tests.
