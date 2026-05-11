# Observations

Append-only log of things noticed during factory usage. Each one is a
potential brief. Promote to a run when ready to act on it.

---

2026-05-09 — Building the factory itself doesn't use `factory run`
because the tool and the thing being built are the same. Consider
whether there's a way to use the factory to modify itself, or whether
self-modification is always manual.

2026-05-10 — Need guidance on writing skills — how to structure them,
what goes in SKILL.md vs references, how to follow the Agent Skills
spec. Currently looking up agentskills.io each time. Should be
captured as a skill or reference in the factory.


2026-05-10 — Need a run report mechanism. After a run completes, the
user has no visibility into what happened without manually reading
review artifacts and git diffs. The build-in-the-factory skill should
present a conversational digest at the end of a run: what reviewers
found, what the author changed, what passed, what's still open.

2026-05-10 — Need a test quality reviewer and a corresponding
write-tests skill. Similar pattern to write-documentation /
review-documentation: the skill guides test authoring, the reviewer
checks test quality. The author would reference the write-tests skill
when writing tests, and the reviewer would check against it.

2026-05-09 — The refine-writing skill at ~/Workspace/skills has
reference files (ai_tells.md, benchmarks.md, sentence_corrections.md,
structural_guidance.md) with much more detail than what was captured
into write-documentation. May want to pull more in later, especially
the sentence corrections as concrete examples.
