2026-06-05 — Always build Factory changes through the Factory lifecycle.
Direct implementation, even for apparently small changes, bypasses the
process this repo is meant to exercise: use the build-in-the-factory
skill, create a run, write the brief/behaviors/approach/plan artifacts,
execute through `factory run`, run reviewers, and land through Factory.
Use observations to record intent and lessons for future runs instead of
holding process context only in chat. Today's Codex sandbox change was
implemented directly and should be treated as process debt before it is
landed.
