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

2026-06-05 — The factory now has local Codex support via the Coder
abstraction: `--coder codex` / `FACTORY_CODER=codex` launches
`codex exec --json --cd <worktree>` and records the selected coder
in run state. This unblocks local no-sandbox runs for Codex. Remaining
agent-support work: verify sandboxed Codex, add Fargate Codex support,
and consider whether Pi or other agents need different prompt/session
behavior beyond the current Coder trait.

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

2026-06-05 — The factory should be able to visually observe terminal
UIs during testing. Launch the dashboard (or any TUI) in a tmux
session, capture the screen with tmux capture-pane, and evaluate
the rendered output. This enables autonomous agents to catch
visual bugs (missing animation, stale status, rendering glitches)
without a human looking at screenshots. This should be a skill —
distributable expertise on how to test terminal user interfaces
using tmux capture and VT100 rendering.

2026-06-05 — The run tab shows "[planned]" for runs that are
actively executing. The source run directory keeps the original
status while the worktree has the current status. The run tab
reads from the source dir, not the worktree. The live_dir fix
applies to the selected run's header but not to the run tab
labels — those read from run.status() which uses the source dir.

2026-06-05 — The dashboard animation still feels sluggish despite
the 100ms render interval. The spinner needs to cycle faster to
feel responsive — consider 50-80ms or a different animation style
that communicates activity more clearly at lower frame rates.

2026-06-05 — Wrote expertise/terminal-ui.md without following the
factory process. A proper run with reviewers would have caught:
missing testing approaches for the expertise itself, unclear
discoverability by authors and reviewers, and whether the content
follows our expertise conventions. Always run expertise and skill
changes through the factory — the skills reviewer exists for this.

2026-06-05 — The parallel run merge failed because we committed
to main while child runs were executing. This suggests main should
be protected — no direct commits while runs are active. Consider a
merge queue: an agent that owns merging to main. Child runs and
regular runs produce branches. The merge queue agent rebases,
merges, and optionally spins up new runs to review the merged
result before it lands on main. This is similar to CI merge queues
but the queue agent can be intelligent — resolving simple conflicts,
running targeted reviews on the merged code, and rejecting merges
that break tests. Direct commits to main would be forbidden while
the queue is active.

2026-06-05 — Rate limit UX needs improvement. When the user hits
Anthropic's usage limit: (1) the dashboard should show a countdown
to next retry, not just a static "Rate limited" label, (2) a
notification should tell the user things paused but aren't broken,
(3) the session loop should respect Retry-After headers rather
than using a fixed 5-minute wait, (4) multiple concurrent runs
should stagger retries to avoid thundering herd on the rate limit.

2026-06-05 — Codex sandbox support needs a focused verification run.
The implementation can wrap Codex with the existing macOS Seatbelt
profile, but this should not be treated as secure until tested end to
end: Codex auth/config access, JSON transcript output, write access
limited to the worktree, no writes outside the sandbox root, and no
regression in credential handling. The Codex CLI also has its own
sandbox/approval model, so the interaction between Codex flags and
outer `sandbox-exec` needs explicit validation.

2026-06-05 — Fargate Codex support is intentionally not implemented
yet. The Fargate path is still Claude-specific: container image,
entrypoint, auth token injection, and session assumptions all target
Claude Code. Codex support likely needs a container image update,
Codex authentication/config strategy, runtime selection in the task
environment, and tests for launch, session loop, upload/download, and
review artifacts. Until then, `factory run --runtime fargate --coder
codex` should fail clearly instead of starting a run that breaks
halfway through.

2026-06-05 — Always build Factory changes through the Factory lifecycle.
Direct implementation, even for apparently small changes, bypasses the
process this repo is meant to exercise: use the build-in-the-factory
skill, create a run, write the brief/behaviors/approach/plan artifacts,
execute through `factory run`, run reviewers, and land through Factory.
Use observations to record intent and lessons for future runs instead of
holding process context only in chat. Today's Codex sandbox change was
implemented directly and should be treated as process debt before it is
landed.

2026-06-05 — The assistant's status updates during Factory runs are
generated by combining run-level state with recent agent transcripts:
the active run id/status, session count, phase transitions from the
factory runner, reviewer verdicts as they appear, current worktree git
status, recent transcript events, review artifacts, and test command
results. Factory already stores most of these inputs in run artifacts
(`status`, `sessions.log`, `sessions/`, `reviews/`, `report.md`) and the
dashboard already parses enough to show run tabs, agents, phases,
activity, reports, and verdicts. A future reporting agent could use the
same inputs to generate concise overall-run updates across all agents:
what phase the run is in, which agents are active or failed, what just
changed, which checks passed, and what remains before landing.

2026-06-05 — The dashboard can show inconsistent and noisy state while a
run is being fixed after review. In one screenshot, the header showed the
active run as `executing` while the run tab still showed the same run as
`[planned]`. The author transcript also rendered command-output styling
artifacts such as `Msh]` and `Mpt:` around `git status`/diff output. The
dashboard should derive the selected run's tab status from the same
effective status used by the header and should sanitize transcript style
fragments so command output remains readable.

2026-06-05 — Formatter churn should be prevented by process, not cleaned
up after the fact. Factory should run the repo's formatter consistently
before merge, ideally as an explicit pre-merge or review gate, so commits
do not carry incidental broad formatting changes from whichever agent or
developer happened to run `cargo fmt` locally. When a run requires
formatting, the formatted diff should be deliberate and reviewer-visible
rather than mixed into unrelated implementation changes.

2026-06-05 — After landing the Codex approval-flag fix, installed smoke
run `20260605-codex-installed-smoke-3` verified the fixed command shape.
The installed Factory binary launched installed Codex without the
`unexpected argument '--ask-for-approval'` parser error; invoking Codex
directly with the old bad order still reproduces that parser error.
When Codex was launched from inside this tool's outer sandbox, it then
failed with `failed to initialize in-process app-server client:
Operation not permitted`, including when called directly with the
correct argument order. Treat that as an environment/sandbox interaction
separate from Factory's flag placement. Earlier failed smoke
`20260605-codex-installed-smoke-2` also exposed a status propagation
gap: the worktree run status was `failed`, while the source run
directory still showed `planned` because failed worktree artifacts were
not copied back.

2026-06-05 — Run `20260605-193223` addressed the dashboard stale-status
and deleted-run observations: run tabs now use the same cached live
status as the header, initial selection prefers live active runs, polling
removes source run directories that disappeared, and the dashboard falls
back to an existing run or the empty state when the selected run is
removed. The run added unit coverage and external dashboard behavior
coverage for those cases.

2026-06-05 — Factory review detection is commit-based. During run
`20260605-193223`, an author wrote valid implementation changes and
marked the run complete, but left the worktree dirty. Factory compared
`main..HEAD`, saw no committed diff, skipped reviews, and produced a
no-code-changes report. The session loop should require or verify a clean
committed worktree before `complete`, or Factory should detect dirty
worktrees and fail/needs-user instead of skipping reviews.

2026-06-05 — Add a `factory version` command that prints the installed
binary version plus the Git commit ID it was built from. After upgrading
or installing Factory, users need a quick way to confirm which source
commit the active binary corresponds to, especially when smoke-testing
recently landed fixes from `~/.local/bin/factory`.

2026-06-05 — Consider turning the `build-in-the-factory` skill into a
slash command or command-style entrypoint. The workflow is now project
policy, not just agent-local guidance, and a slash command could make the
same brief creation, run setup, review, observation, and landing process
available across agents that support commands. This may reduce drift
between Claude, Codex, and future coders by giving each agent the same
Factory-native starting point instead of relying on whether it loaded the
skill text into context.

2026-06-05 — Local run filesystem sandboxing should allow exactly the
run worktree plus the source repository's common git directory, not the
entire workspace parent. Factory currently needs the agent to write
linked-worktree git metadata under the source repo's `.git/worktrees`
while working from the run worktree. Granting the whole parent directory
is pragmatic but too broad because it can also expose `main/` and sibling
runs. A focused run should compute the common git dir, render Seatbelt
and Codex writable roots from `run worktree + common git dir`, and add
behavior tests proving sandboxed Claude and Codex runs can commit without
being able to write unrelated sibling worktrees.

2026-06-05 — Network policy is a separate sandbox design axis from
filesystem roots. Local Seatbelt currently allows outbound network, but
stricter modes or Codex's internal sandbox may deny or constrain network
access. That can break dependency workflows such as package install,
registry metadata lookup, crate/npm/pip downloads, and tool/model
bootstrap. Explore whether Factory should support project-configurable
network policy, dependency-cache writable/read-only mounts, allowlisted
install phases, or explicit dependency setup runs so agents can build
projects without silently weakening credential and filesystem isolation.

2026-06-06 — Observation discussion, run scheduling, run execution, and
landing can be decoupled into separate loops. The human discussion loop
can happen whenever the human is available: review open observations,
shape briefs/behaviors/approaches/plans, and queue a batch of runs. A
run queue can then execute scheduled runs autonomously, choosing Codex,
Claude, local, or Fargate capacity to maximize available subscription and
runtime resources. The scheduler can use run priority, coder/runtime
availability, subscription limits, expected duration, reviewer load, and
dependency/network needs as inputs so scarce agent capacity is consumed
on ready work instead of waiting for the human discussion loop. Completed
runs can enter an independent merge queue that rebases, runs checks, runs
or verifies reviews, lands eligible branches, and handles conflicts. Some
runs will still end in
`needs-user`, but those should return to the human discussion queue
rather than blocking unrelated scheduled work or mergeable completed
runs.

Architecturally, separate these roles:

- Observation queue: raw ideas, incidents, and lessons. Cheap to append,
  not yet scheduled.
- Planning queue: observations that have been discussed enough to become
  briefs/behaviors/approaches/plans. Human-heavy, can happen in batches.
- Run queue: approved planned runs waiting for coder/runtime capacity.
  Machine-heavy, scheduled against Codex/Claude limits.
- Review queue: completed author work waiting for reviewers or reruns.
- Merge queue: reviewed branches waiting for rebase/check/land, with
  conflict handling and possible follow-up runs.
- Needs-user queue: runs that cannot progress autonomously, returned to
  the human discussion loop rather than blocking the run or merge queues.

The subtle win is that "human availability" and "subscription capacity"
become independently optimized resources.

Open design question: the run queue and review queue may not need to be
separate implementation queues because authoring and review form a
loop. Treat them as separate conceptual roles for now, but revisit the
boundary when implementing the workflow.

Observation sources do not have to be human-only. A live system can log
observations from telemetry, failing checks, flaky-test analysis,
production incidents, or analysis that points at a likely bug area.
Those system-generated observations can enter the same discussion and
planning flow as human notes. Similarly, the merge queue should be able
to land learnings, not only code: expertise updates, behavior mappings,
documentation corrections, and other durable project memory can be
reviewed and landed through the same queue.

The same structure should also support teams, not only one human
operator. Different people can populate observations, discuss and shape
plans, approve scheduled runs, review completed work, and operate the
merge queue independently. The queue boundaries create parallelism for
human attention as well as for agent/runtime capacity.

In that architecture, the Factory dashboard becomes the observability
surface for all of these queues: observation inflow, planning state, run
capacity, review loops, merge readiness, needs-user items, telemetry
signals, and landed learnings. It may also become the intervention
surface where any observing human can act on a cue. That likely needs a
permission model over time, so different humans can be allowed to
observe, triage, approve runs, restart runs, resolve needs-user items,
or land changes at different levels.
