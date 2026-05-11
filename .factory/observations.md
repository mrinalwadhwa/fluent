# Observations

Append-only log of things noticed during factory usage. Each one is a
potential brief. Promote to a run when ready to act on it.

---

2026-05-09 — define-behaviors skill broke its own rule during the
documentation reviewer run. It walked through accuracy, writing quality,
and completeness one at a time (good), then dumped review output,
triggering, and loop behaviors all at once (bad). The skill should
maintain the one-area-at-a-time pace through to the end.

2026-05-09 — design-approach skill had the same problem. Walked through
decisions one at a time (good), then dumped the full approach document
instead of discussing solution outline and risks incrementally (bad).

2026-05-09 — Building the factory itself doesn't use `factory run`
because the tool and the thing being built are the same. Consider
whether there's a way to use the factory to modify itself, or whether
self-modification is always manual.

2026-05-10 — Need guidance on writing skills — how to structure them,
what goes in SKILL.md vs references, how to follow the Agent Skills
spec. Currently looking up agentskills.io each time. Should be
captured as a skill or reference in the factory.

2026-05-10 — Skills that should reference expertise: design-approach
(structural decisions), plan-execution (step ordering, slicing),
define-behaviors (domain vocabulary only). Update these skills to
point to relevant expertise files once the architecture expertise
is populated.

2026-05-10 — The expertise layer has been designed. Two layers:
factory-level (expertise/ in the factory repo) and project-level
(expertise/ in the project repo). Skills reference expertise files
for decision-making context. Expertise is what you know; skills are
what you do. Still need to build it and migrate existing embedded
guidance (writing quality from write-documentation, thinking
frameworks from capture-brief) into expertise files.

2026-05-10 — Need a run report mechanism. After a run completes, the
user has no visibility into what happened without manually reading
review artifacts and git diffs. The build-in-the-factory skill should
present a conversational digest at the end of a run: what reviewers
found, what the author changed, what passed, what's still open. The
user can ask follow-up questions. Commit messages also need guidance
in the factory — the author agent's commit messages during the review
run were action-focused ("Run X and fix Y") rather than change-focused
("Improve X and add Y"). Body should use bullet points.

2026-05-10 — First full-codebase review run worked end-to-end. Two issues:
(1) The author agent added a Co-Authored-By line despite CLAUDE.md
prohibiting it. The system prompt or skill needs to reinforce this.
(2) The author committed directly to main because cmd_run_bare doesn't
create a worktree. Review runs via --no-sandbox skip worktree creation.
Need to ensure review runs still get worktree isolation.

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
