2026-06-12 — Manage observations as independent files instead of one
monolithic `observations.md`. Each observation gets its own file
(e.g., `.factory/observations/<id>.md`) with its own filename-anchored
ID. Resolved observations move to `.factory/observations/resolved/`
(or equivalent sibling folder) rather than concatenating into a
shared `observations-resolved.md`. This reduces the chance of
conflicts on observation files — two Work Items that both add
observations land in different files and never compete for the same
git anchor.

Trigger: the additive conflict in `.factory/observations-resolved.md`
that blocked the `fargate-teardown-command` merge (recorded in the
agentic-rebase observation below) happened because two Work Items
each appended a "Resolved" block to the same shared file at the same
anchor line. Per-file observations would have made that conflict
impossible.

Open questions for the brief:
- ID format (timestamp-kebab matching Factory's other conventions
  vs. content-hashed vs. user-chosen?).
- Migration path for the existing concatenated files (one-shot
  split vs. accept-as-is and apply per-file going forward).
- Index/listing UX so the open queue is still scannable as a flat
  list (auto-generated `INDEX.md` from filenames + first lines?
  CLI command? Both?).

→ Resolved: Implemented by the per-file-observations Work Item. Observations now use per-file layout under .factory/observations/ with CLI commands for add, resolve, list, show, and migrate.
