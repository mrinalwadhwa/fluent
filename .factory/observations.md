# Observations

Append-only log of things noticed during factory usage. Each one is a
potential brief. Promote to a run when ready to act on it.

---

2026-05-11 — During the interactive stages, there were loops where
the user just typed "yes, keep going" repeatedly. These indicate
steps that are potentially automatable and may not need a human in
the loop. The factory should learn from these patterns to reduce
unnecessary pauses.

2026-05-11 — For the last several iterations, we stopped using the
brief-based full factory workflow. This might be because we're deep
into a long session (700k+ tokens of 1M window) and the flow is
affected by context pressure. Or it might be that these small changes
genuinely didn't need the full workflow. Worth distinguishing between
the two causes.

2026-05-11 — To distribute the factory, we need a binary (the shell
script isn't sufficient) and a way to distribute factory-level skills
and expertise. This is a big change. Prerequisites: good testing
setup to guard against regression, skill writing guidance/expertise
and a skill reviewer, test writing guidance/expertise and a test
reviewer. All of these improve coverage before we risk breaking core
functionality with a major structural change.

2026-05-11 — Review runs leave worktrees behind after completion.
The factory script should clean up worktrees for completed review
runs, or factory status should show orphaned worktrees so the user
knows to clean them up.

2026-05-12 — The run lifecycle has six stages: isolate → execute →
review → land → capture → cleanup. The current factory stops at
review — landing (merge), capture (sync metadata for learning), and
cleanup (remove worktree) are missing. The lifecycle splits at
"planned" into two phases: local/interactive (brief through plan,
needs real-time conversation) and remote/autonomous (execute through
cleanup, can run on Fargate or GitHub). In a future GitHub-driven
workflow, branch = isolation, PR = landing + capture (not for
discussion — too slow), branch deletion = cleanup. A PR-based
discussion loop is too slow for interactive skills. Cleanup should
only happen after landing, not just after completion — a completed
run still has unmerged changes. Failed runs keep their worktree for
debugging.

2026-05-12 — Design knowledge and project-level learnings stored in
Claude Code's memory (~/.claude/projects/.../memory/) are opaque to
the factory and coupled to Claude Code's design. Project knowledge
should live where the factory can consume it. Claude memory is for
user preferences and session continuity. Design decisions, architecture,
and conventions belong in the project (observations → expertise →
documentation lifecycle).

2026-05-09 — Building the factory itself doesn't use `factory run`
because the tool and the thing being built are the same. Consider
whether there's a way to use the factory to modify itself, or whether
self-modification is always manual.


2026-05-09 — The refine-writing skill at ~/Workspace/skills has
reference files (ai_tells.md, benchmarks.md, sentence_corrections.md,
structural_guidance.md) with much more detail than what was captured
into write-documentation. May want to pull more in later, especially
the sentence corrections as concrete examples.
