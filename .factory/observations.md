# Observations

Append-only log of things noticed during factory usage. Each one is a
potential brief. Promote to a run when ready to act on it.

---

2026-05-11 — During the interactive stages, there were loops where
the user just typed "yes, keep going" repeatedly. These indicate
steps that are potentially automatable and may not need a human in
the loop. The factory should learn from these patterns to reduce
unnecessary pauses.

2026-05-12 — The define-behaviors skill should read existing behaviors
from documentation/behaviors.md before writing new ones. This would
calibrate the level of behavioral definition (what's too detailed,
what's too abstract) and avoid duplicating behaviors that already exist.
Currently the skill only reads the brief and codebase, not the existing
behavioral contract.

2026-05-12 — Consider whether there are other interactive git operations
that could block headless agents beyond commit signing (merge conflict
resolution, gpg passphrase prompts, interactive rebase).

2026-05-13 — On the Fargate test, round 2 reviewers all crashed
(exit 1) after round 1 had 5 reviewers + author session 2. Cause
unknown — could be rate limits, container resource exhaustion, or
something else. Needs investigation with reviewer transcripts next
time it happens.

2026-05-15 — The sandbox allows outbound network, so a malicious
package's postinstall script could exfiltrate workspace contents via
HTTP. The sandbox prevents credential theft and privilege escalation
but not data exfiltration. Options: (A) network proxy allowlisting
API endpoints only, (B) deny outbound except localhost with credential
proxy mediating all API access, (C) read-only package caches. Option
B aligns with isolation-by-impossibility principle.

2026-05-09 — The refine-writing skill at ~/Workspace/skills has
reference files (ai_tells.md, benchmarks.md, sentence_corrections.md,
structural_guidance.md) with much more detail than what was captured
into write-documentation. May want to pull more in later, especially
the sentence corrections as concrete examples.

2026-05-16 — Interactive skills (capture-brief, define-behaviors,
design-approach, plan-execution) have no test scenarios. These are
non-trivial skills that drive the planning phase. Testing them requires
designing scenario-based tests that simulate the interview flow and
verify outputs. Tracked in behaviors.md as "(needs scenarios)" but
warrants its own run.

2026-05-16 — The notification system (macOS osascript notifications
from factory watch) needs a purpose review. What value do notifications
add to the workflow? When are they useful vs noise? Should they be
richer (actionable, with run context) or replaced by something else
(dashboard focus, sound, status bar)?

2026-05-16 — Complementary "create" skills needed: architect (pairs
with review-architecture), write-tests (pairs with review-tests),
write-documentation (pairs with review-documentation), write-skill
(pairs with review-skills). Each shares expertise via references/
symlinks with its review counterpart.

2026-05-18 — Create a skill for browsing the web using agent-browser
as a fallback when WebFetch/curl fail (Medium, paywalled sites,
JS-rendered pages). Also create a skill for fetching YouTube video
transcripts using yt-dlp (fetch auto-generated captions, clean VTT
into readable text).

2026-06-05 — Create a skill for generating PDFs using Typst. Typst
is a modern typesetting system (alternative to LaTeX) that compiles
markup to PDF. A skill could teach agents to write Typst documents
for resumes, reports, invoices, or any structured document that
needs PDF output. Reference Claude Code history for threads that use
Typst.

2026-06-05 — The plan phase identifies parallelizable steps but
the factory has no mechanism to execute them in parallel. A plan
that says "step 1a, 1b, 1c are independent" still runs as a single
serial session. The factory should support decomposing a plan into
parallel child runs — create separate run directories for each
parallel step, launch them simultaneously, and gate the next step
on all completing.

2026-06-05 — How does the factory learn? Expertise files are
manually written. Observations are manually captured. Decisions
are manually recorded. There's no mechanism for the system to
accumulate knowledge from runs automatically. Review findings,
author mistakes, production incidents — these could feed back
into expertise and decisions without human curation. The lifecycle
has "capture" as a phase but it's not implemented beyond copying
artifacts. What does automated knowledge capture look like?

2026-06-05 — The factory currently only supports Claude Code as
the coding agent. It should support other agents: OpenAI Codex,
Pi, and potentially others. The Coder trait already abstracts
the agent interface (run, run_interactive), but the implementations
(SandboxedClaudeCode, BareClaudeCode) are Claude-specific. Need
to design how agent selection works, how prompts/flags differ per
agent, and whether the session loop needs agent-specific behavior.

2026-06-05 — Dashboard "reviewing" status shows no spinner in the
header. compute_phase needs to map "reviewing" to animated=true.
Also, reviewer tabs show stale verdicts from the previous round
instead of resetting to "running" when a new review round starts.
The dashboard needs to detect that review artifacts have been
archived (moved to round-N/) and reset reviewer status accordingly.

2026-06-05 — When a run completes, the dashboard should show the
run's report (report.md) in the activity feed or a dedicated pane.
Currently a completed run shows the last author session's transcript
which ends with "Session complete." The report summarizes what
happened across all sessions and review rounds — that's what the
user wants to see when checking on a finished run.

2026-06-05 — The author-reviewer loop can be faster without
skipping reviewers. All reviewers still run every round, but
with scoped prompts: reviewers that passed last round get "your
previous verdict was pass, these files changed, re-evaluate only
if relevant to your domain." Reviewers that failed get "here are
your findings, here's what the author changed, re-evaluate."
The factory can derive this from the diff and previous verdicts
without author input. The author's handoff explains what changed
and why, which naturally scopes the review.

2026-06-05 — Quality over speed in the review loop. Don't optimize
review time at the expense of thoroughness. Scoped review prompts
should provide context (previous verdict, what changed) to help
reviewers focus, not to reduce their coverage. A reviewer that
passed last round should still re-evaluate fully if the changes
could affect its domain. The goal is better-informed reviewers,
not faster ones. Reviewers should always view what the author
says with skepticism — the author's explanation of what changed
is context, not evidence. The reviewer verifies independently.

2026-06-05 — The dashboard needs much more animation to signal
activity. Currently only the header phase label animates. Should
also animate: active agents in the agent tabs (spinner next to
name), active runs in the run tabs (spinner next to status),
and the "reviewing" status. Active runs and agents should sort
first in their respective lists. Consider a global activity
indicator in the dashboard title bar when any run is active.
The dashboard should feel alive when work is happening and
completely still when everything is done.

2026-06-05 — The dashboard never removes runs that were deleted
from disk. App::poll discovers new runs but never prunes stale
ones. If a run directory is removed while the dashboard is open,
the run stays in the list with "[-]" status forever. Poll should
remove runs whose directories no longer exist.
