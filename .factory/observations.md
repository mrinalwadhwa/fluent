# Observations

Append-only log of things noticed during factory usage. Each one is a
potential brief. Promote to a run when ready to act on it.

---

2026-05-09 — cmd_run_local and cmd_run_bare have duplicated session loop
logic (snapshot capture, status checking, review phase). The differences
are small (sandbox + credential refresh vs --dangerously-skip-permissions).
Extract the loop body into a shared function.

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

2026-05-10 — First full-codebase review run worked end-to-end. Two issues:
(1) The author agent added a Co-Authored-By line despite CLAUDE.md
prohibiting it. The system prompt or skill needs to reinforce this.
(2) The author committed directly to main because cmd_run_bare doesn't
create a worktree. Review runs via --no-sandbox skip worktree creation.
Need to ensure review runs still get worktree isolation.

2026-05-10 — Full-codebase reviews should be runs, not a separate
command. The worktree isolation and history are valuable. But the
full brief → behaviors → approach → plan ceremony is heavy for what's
essentially "run all reviewers." Need a lightweight run path — a brief
that says "full review" should skip empty stages and go straight to
execution. Resolve this in the capture-brief or build-in-the-factory
skill.

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
