# Resolved Observations

Observations that have been acted on. Kept for potential pattern
analysis later.

---

2026-05-09 — cmd_run_local and cmd_run_bare have duplicated session loop
logic (snapshot capture, status checking, review phase). The differences
are small (sandbox + credential refresh vs --dangerously-skip-permissions).
Extract the loop body into a shared function.
→ Resolved: 99c252e (deduplicated into run_session_loop)

2026-05-10 — Full-codebase reviews should be runs, not a separate
command. The worktree isolation and history are valuable. But the
full brief → behaviors → approach → plan ceremony is heavy for what's
essentially "run all reviewers." Need a lightweight run path — a brief
that says "full review" should skip empty stages and go straight to
execution. Resolve this in the capture-brief or build-in-the-factory
skill.
→ Resolved: 26e2ada (review runs with mode=review skip to planned)

2026-05-09 — define-behaviors skill broke its own rule during the
documentation reviewer run. Dumped review output, triggering, and loop
behaviors all at once instead of one area at a time.
→ Resolved: pacing rule reinforced in define-behaviors and design-approach

2026-05-09 — design-approach skill had the same problem. Dumped full
approach document instead of discussing incrementally.
→ Resolved: pacing rule reinforced in design-approach

2026-05-10 — Skills should reference expertise files (design-approach,
plan-execution). Expertise layer needed for writing quality guidance.
→ Resolved: design-approach and plan-execution reference
expertise/architecture/principles.md. write-documentation moved to
expertise/writing/documentation.md.
