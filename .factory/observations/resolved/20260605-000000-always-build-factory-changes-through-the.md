2026-06-05 — Always build Factory changes through the Factory lifecycle.
Direct implementation, even for apparently small changes, bypasses the
process this repo is meant to exercise: use the build-in-the-factory
skill, create a run, write the brief/behaviors/approach/plan artifacts,
execute through `factory run`, run reviewers, and land through Factory.
Use observations to record intent and lessons for future runs instead of
holding process context only in chat. Today's Codex sandbox change was
implemented directly and should be treated as process debt before it is
landed.

→ Resolved: Superseded by codified discipline. CLAUDE.md ('use the factory to build the factory') and the MEMORY.md feedback file (feedback_skip_factory_when_too_slow.md) now carry the rule and its named exception. The specific Codex sandbox change referenced has long landed via other paths. Vocabulary is also legacy (factory run, create a run) — Work model uses factory work attempt run.
