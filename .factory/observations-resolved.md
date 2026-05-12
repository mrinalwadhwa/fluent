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

2026-05-11 — Fargate entrypoint duplicated session loop, review
functions, report generator, and system prompt from factory script.
→ Resolved: entrypoint sources factory script via FACTORY_LIB=1.

2026-05-10 — Need guidance on writing skills. Keep looking up
agentskills.io each time.
→ Resolved: added expertise/skills.md with Agent Skills spec patterns,
skill design guidance, and lessons learned from building factory skills.

2026-05-10 — Need a test quality reviewer and write-tests expertise.
→ Resolved: added expertise/tests.md with testing principles (behavior
vs implementation, test levels, design techniques, anti-patterns) and
review-tests skill.

2026-05-10 — Author agent added Co-Authored-By despite CLAUDE.md and
wrote process-focused commit messages.
→ Resolved: expanded CLAUDE.md commit guidance with examples, added
commit rules to factory system prompt.

2026-05-10 — Review runs via --no-sandbox skip worktree creation.
Author commits directly to main.
→ Resolved: cmd_run_bare creates worktree when in a git repo (local).
Skips on Fargate where there's no git repo.

2026-05-11 — Three of four reviewers printed results to stdout but
didn't write the review artifact file during the latest review run.
The verdict check defaulted to pass.
→ Resolved: run_single_reviewer now cds to the project root derived
from the run dir before launching claude. Reviewers were writing
artifacts at relative paths that resolved to the original project
root instead of the worktree.

2026-05-11 — The author agent's skill is mostly about referencing
expertise. It should know about expertise and draw on it.
→ Resolved: added expertise section to FACTORY_SYSTEM_PROMPT listing
factory-level (expertise/) and project-level (.factory/expertise/)
reference material. Also fixed duplicate Session start heading.

2026-05-12 — The system prompts (FACTORY_SYSTEM_PROMPT, reviewer prompts)
are embedded in the factory shell script.
→ Resolved: extracted to prompts/ directory. Author prompt in
prompts/author.md. Reviewer prompts in prompts/review-{name}.md with
[system], [full-codebase], [run-scoped] sections. Reviewer loop in
run_reviews collapsed from 5 blocks to a single loop. PROMPTS_DIR
overridable for FACTORY_LIB sourcing.
