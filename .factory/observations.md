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

2026-05-12 — The launch_agent function only launches the author agent,
never reviewers (those go through run_single_reviewer). The name
"agent" is vague — should be launch_author to make clear which agent
is being launched. Apply the same naming clarity to other places where
"agent" is used generically.

2026-05-12 — The define-behaviors skill should read existing behaviors
from documentation/behaviors.md before writing new ones. This would
calibrate the level of behavioral definition (what's too detailed,
what's too abstract) and avoid duplicating behaviors that already exist.
Currently the skill only reads the brief and codebase, not the existing
behavioral contract.

2026-05-12 — Consider whether there are other interactive git operations
that could block headless agents beyond commit signing (merge conflict
resolution, gpg passphrase prompts, interactive rebase).

2026-05-12 — The run was initially launched with `| head -20` which
SIGPIPE'd the factory process. Factory output (session banners, status
updates) goes to stdout, same as the agent's print-mode output. Piping
factory run output is destructive. The factory should either write its
own output to stderr, or log to a file by default so stdout is safe
to pipe or discard.

2026-05-12 — The factory's interface should be richer than a
traditional CLI. The interactive phases drive conversations, autonomous
phases show parallel progress, and the factory mediates between user
and agent. A TUI is the natural fit — ratatui + crossterm in Rust.
Start minimal (status bar, session indicators, reviewer progress),
evolve toward split panes (conversation + status + progress). Web UI
is future but don't over-engineer for it now. This should be its own
run after the Rust port.

2026-05-13 — The run-local plan assumed the scaffolding run would
produce stubs, but it produced full implementations. The author
correctly identified there was no remaining work beyond the rename.
Lesson: verify what previous runs actually delivered before planning
follow-up runs. Don't assume a plan's steps are needed without
checking the current state.

2026-05-13 — The scaffolding run produced code that used Agent/Sandbox/
Backend despite the approach explicitly specifying Coder/Os/Runtime.
This happened before reviewer verdict criteria were tightened — the
architecture reviewer passed with "advisory" findings. Now that
verdicts are stricter, this would be caught. The run-local run
successfully fixed the naming in one session.

2026-05-13 — On the Fargate test, round 2 reviewers all crashed
(exit 1) after round 1 had 5 reviewers + author session 2. Cause
unknown — could be rate limits, container resource exhaustion, or
something else. Needs investigation with reviewer transcripts next
time it happens.

2026-05-13 — Run-scoped reviewers on trivial runs (e.g. a test brief
that just writes complete) still find issues with the broader codebase.
The scope should be narrower — if the run produced no code changes,
reviewers should pass immediately or be skipped.

2026-05-13 — capture_snapshot copies from ~/.claude/ which is the
global Claude Code state, not the run's session. It captures history
from all sessions, memory from the first project found (not
necessarily this one), and todos/plans from all agents. None of this
is specific to the factory run. The snapshot should either: capture
only the run's agent session (requires Claude Code to expose per-
session state), or filter to relevant content, or be dropped entirely
until a proper mechanism exists.

2026-05-09 — The refine-writing skill at ~/Workspace/skills has
reference files (ai_tells.md, benchmarks.md, sentence_corrections.md,
structural_guidance.md) with much more detail than what was captured
into write-documentation. May want to pull more in later, especially
the sentence corrections as concrete examples.
