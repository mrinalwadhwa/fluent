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

2026-05-13 — All factory runs so far have used --no-sandbox. The
sandbox exists to prevent agents from doing destructive things on
the user's machine (bad dependencies, file deletion outside
workspace). For autonomous operation on a primary computer, the
sandbox should be the default, not an opt-in. This means the Rust
binary's sandbox support needs to work reliably. Getting to
"always sandboxed" is prerequisite for trust in autonomous runs.

2026-05-13 — Visibility into running sessions is critical for trust.
Currently a background run is opaque until it finishes. The user
can tail a log file but that's raw agent output. What's needed is a
way to see: which step the agent is on, what files it's changing,
whether tests are passing, how much context is used. This might
elevate the TUI work from "nice to have" to prerequisite for
real autonomous use. Alternatively, a simpler mechanism — a live
status file the factory updates with current activity — could
provide visibility without a full TUI.

2026-05-13 — The installed factory binary at ~/.local/bin/factory gets
SIGKILL'd (exit 137) by macOS due to com.apple.provenance extended
attribute on unsigned binaries. Fix: ad-hoc sign with codesign -s -
after copying. The install step (cargo build --release + cp) needs to
include signing. For Homebrew distribution, the formula handles this.

2026-05-13 — The Rust binary's factory watch command spawns background
processes (polling at 1s, 2s, 10s, 60s intervals) that are never
cleaned up when the parent run finishes. Every run leaks 3+ watch
processes. After a session of runs, ~70 orphaned watch processes were
found running from deleted worktree binaries. These need to be killed
when the run completes, or watch should not spawn background processes.

2026-05-13 — The Rust binary's observability features were added by
an autonomous run but don't fully work at runtime despite unit tests
passing. sessions.log isn't written, transcript.jsonl captures old
~/.claude/history.jsonl instead of stream-json output, and review
round archives aren't created. The unit tests mock the behavior but
don't verify the actual subprocess piping or file output. This is a
case where the replication anti-pattern applies — tests verify a
model of the behavior, not the real behavior. Needs integration tests
that run the binary and check the actual artifacts produced.

2026-05-13 — capture_snapshot copies from ~/.claude/ which is the
global Claude Code state, not the run's session. It captures history
from all sessions, memory from the first project found (not
necessarily this one), and todos/plans from all agents. None of this
is specific to the factory run. The snapshot should either: capture
only the run's agent session (requires Claude Code to expose per-
session state), or filter to relevant content, or be dropped entirely
until a proper mechanism exists.

2026-05-15 — The sandbox allows outbound network, so a malicious
package's postinstall script could exfiltrate workspace contents via
HTTP. The sandbox prevents credential theft and privilege escalation
but not data exfiltration. Options: (A) network proxy allowlisting
API endpoints only, (B) deny outbound except localhost with credential
proxy mediating all API access, (C) read-only package caches. Option
B aligns with isolation-by-impossibility principle.

2026-05-15 — Dashboard auto-scroll should re-enable when the user
scrolls to the bottom. Currently once disabled it stays off until
the user switches agents or runs.

2026-05-15 — Dashboard field refresh is inconsistent. Reviewer status
colors don't always update, phase/status in header can be stale, run
list statuses don't refresh. All displayed fields need to be
re-evaluated on each poll cycle to reflect current state. The
dashboard should make it obvious when something changes.

2026-05-15 — The content resolver looks for sandbox profiles at
sandbox/common.sb and sandbox/claude-code.sb but the files lived at
~/.config/factory/common.sb (no sandbox/ subdirectory). Had to copy
them to ~/.config/factory/sandbox/. The resolver path and the actual
file location should match.

2026-05-09 — The refine-writing skill at ~/Workspace/skills has
reference files (ai_tells.md, benchmarks.md, sentence_corrections.md,
structural_guidance.md) with much more detail than what was captured
into write-documentation. May want to pull more in later, especially
the sentence corrections as concrete examples.
