# Observations

Open queue of things noticed during factory usage. Each one is a
potential brief. When an observation is resolved, move it to
observations-resolved.md with the resolution context.

---

2026-05-11 — During the interactive stages, there were loops where
the user just typed "yes, keep going" repeatedly. These indicate
steps that are potentially automatable and may not need a human in
the loop. The factory should learn from these patterns to reduce
unnecessary pauses.

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
JS-rendered pages).

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

2026-06-05 — Wrote expertise/terminal-ui.md without following the
factory process. A proper run with reviewers would have caught:
missing testing approaches for the expertise itself, unclear
discoverability by authors and reviewers, and whether the content
follows our expertise conventions. Always run expertise and skill
changes through the factory — the skills reviewer exists for this.

2026-06-07 — PDF and YouTube expertise were manually merged into `main`
from a Claude session in commit `c07ddb7` (`Add PDF and YouTube
expertise`). Treat this like the earlier terminal-UI expertise case:
useful expertise can arrive through direct human/assistant collaboration,
but future expertise changes should normally go through the Factory
lifecycle so skill, documentation, architecture, and behavior reviewers
can check discoverability, reference paths, quality, and testability.

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

2026-06-07 — Factory discussion agents should frame clarification
questions so they are easy for the user to answer. Prefer concrete,
answerable prompts over open-ended phrasing when the discussion has
already converged on a likely direction. For example, ending with "Does
that make sense?" or "Should we use this as the default?" lets the user
answer "yes" and keep momentum, while broad questions often force the
user to reconstruct the whole design context before responding.

2026-06-07 — Running Factory Codex child sessions from inside a Codex
conversation still fails when the outer Codex session is launched with
restricted network/app-server permissions, even if the filesystem roots
allow sibling worktrees. `factory --no-sandbox resume
20260607-183819-attempt-intake --coder codex` bypassed Factory's
Seatbelt wrapper, but nested `codex exec` failed immediately with
`failed to initialize in-process app-server client: Operation not
permitted`. `codex doctor` in the same shell reported restricted
network, unreachable ChatGPT endpoints, and an idle app-server. This is
separate from worktree permissions: the conversation-hosted Codex agent
needs a launch mode that allows the delegated Codex runtime to initialize
and reach the model endpoint, or Factory needs a different Codex
execution surface for nested runs.

Related CLI footgun: `factory resume --no-sandbox ...` currently treats
`--no-sandbox` as an extra agent argument because `resume` only reads the
top-level `factory --no-sandbox resume ...` flag. `resume` should accept
the same local runtime flags as `run` so recovery commands do what users
expect.

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

2026-06-05 — Consider turning the `build-in-the-factory` skill into a
slash command or command-style entrypoint. The workflow is now project
policy, not just agent-local guidance, and a slash command could make the
same brief creation, run setup, review, observation, and landing process
available across agents that support commands. This may reduce drift
between Claude, Codex, and future coders by giving each agent the same
Factory-native starting point instead of relying on whether it loaded the
skill text into context.

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
surface for humans with permission to unblock or steer the appropriate
queue.

2026-06-07 — Queue/core-machinery redesign discussion has converged on
these terms and boundaries so far. A **Work Item** is the durable intent
and contract: what Factory is trying to accomplish and why. An
**Attempt** is one bounded try or phase to satisfy a Work Item; attempts
are visible in state and dashboard but usually are not their own queue.
A **Task** is a schedulable unit of agent or machine work. A
**Workspace** is a Factory-managed filesystem/git execution context; a
task may read many workspaces but write to at most one, and same-attempt
follow-up writer tasks usually write the same workspace one at a time.

Planning should primarily operate on Work Items and produce versioned
artifacts: brief, behaviors, approach, and plan. Attempts snapshot the
artifact versions they use, including brief and behavior versions, so
failed or superseded attempts remain interpretable. Plan expansion should
create a concrete task graph before execution. Review scope is defined
up front by explicit policy, then inferred from domain/changed files,
then default reviewers; authors do not decide what gets reviewed.

Reviews should be read-only with respect to candidate workspaces for all
reviewers. Review tasks may write only to their own task artifact
directory: verdicts, findings, suggested patches, scratch tests, or
other review artifacts. If a review produces explicit suggested changes
or scratch tests, or a concrete actionable finding, Factory should
automatically create follow-up writer tasks. Vague concerns should become
`needs-user` or return to planning unless the reviewer can make them
concrete. Separate writer tasks give provenance and scheduler control,
but they do not imply separate workspaces by default.

Keep the existing Factory vocabulary around **behaviors** rather than
introducing "criteria" as a first-class noun. `define-behaviors` is a
planning activity that defines observable behavior before authoring;
`review-behaviors` is a post-author review task that verifies behavior
from the external interface. Concrete behavior tests often cannot be
written until the external surface is stable. In the redesigned model,
behavior reviewers should not directly modify the candidate workspace;
passing scratch tests or suggested checks should feed an automatic
follow-up writer task that adopts durable behavior tests when needed.

The author/review loop should be driven by review policy over candidate
workspace changes, not by author discretion. During an attempt, rerun
only affected reviewers when possible: reviewers whose finding caused the
follow-up, whose file/domain/artifact dependencies changed, whose scope
was explicitly requested, or whose domain may be affected by broad/shared
changes. A previous pass can help the attempt loop continue, but should
be tracked as stale if relevant changes occurred after that review. The
merge queue should run the full required reviewer set before landing,
especially because rebases can compound effects. Merge-time reviewers
should receive focused context about the work item, attempt history,
diffs, and relevant changes so they do not have to review the entire
codebase from scratch, but the full reviewer set remains the safety gate.

The first implementation can keep backward compatibility with the
existing `.factory/runs` lifecycle as a temporary bridge. Long term, once
the new Work Item / Attempt / Task model is working, Factory should
remove that compatibility layer instead of carrying both models
indefinitely. The compatibility bridge is useful for incremental landing,
but it should not become permanent architecture.

Use small generic task kinds with domain-specific roles. Core task kinds
should stay close to scheduler capabilities: `write`, `review`, `merge`,
`report`, `learn`, and `probe`. Roles and prompts carry the specific
purpose: primary author, review fix, behavior-test adoption,
architecture review, documentation review, merge-conflict resolution,
run summary, learning synthesis, and so on. Primary authoring and
follow-up writing are the same `write` machinery; they differ mostly in
prompt, inputs, and scope.

In the new model, a `write` task must never move forward with
uncommitted changes in its writable workspace. Completing a write task
means the task produced an explicit commit, or explicitly failed/needs
user. Dirty workspace state is not a valid boundary between write and
review/merge because it blurs provenance, makes review inputs unstable,
and lets later automation accidentally reinterpret unfinished work. The
current `.factory/runs` dirty-complete review behavior is a compatibility
bridge, not the target behavior for Work Item / Attempt / Task execution.

Runs `20260607-130441-work-model-storage` and
`20260607-164512-work-cli` reinforced several parts of this boundary. The
old `.factory/runs` loop let review-owned tests enter the candidate
workspace, then had to restart the author to commit the reviewed dirty
state before completing. In the new model, review tasks should write only
review artifacts and create follow-up writer tasks for durable code/test
changes. The runs also showed source run state can remain stale
(`planned`) while the live worktree has completed state; new scheduling
and landing should rely on one durable Work Item/Attempt source of truth
instead of copying status across run directories. Finally, behavior
review briefly treated a broad `cargo test --test binary` hang as an
uncertain verdict because the suite launched a real nested agent session.
Review/regression checks should use hermetic test doubles or explicit
timeouts for nested agent/session behavior so autonomous runs do not
stall on an accidental real coder invocation.

Run `20260607-173454-work-intake` reinforced the same invariant with a
cleaner example. The author repeatedly marked the run complete while the
candidate worktree still had uncommitted product changes. Reviewers then
had to compensate for `git diff main...HEAD` being empty by inspecting
the working-tree diff directly; some reviewers handled that, but the
review input was unstable and slower than necessary. The dirty-complete
compatibility guard eventually restarted the author, produced commit
`c4478c9`, and reran reviews successfully before landing. In the new
Work Item / Attempt / Task model, Factory should enforce this earlier:
a `write` task cannot enter review until its writable workspace is clean
and its candidate changes are represented by one or more commits.

Separate distributed Factory source knowledge from project-local Factory
state. Expertise in the Factory source tree ships with Factory and is
available to projects Factory manages, so changes to source-level
expertise should go through normal Work Item/Attempt/Task/Merge flow.
Expertise inside a project's `.factory` directory is local adaptive
memory for that project and may be updated directly by learning flows.
Because Factory itself is managed by Factory, this repository can also
have local learned state inside its own `.factory` directory that is
distinct from distributed source expertise.

Learning should not be left only to `learn` tasks. Many agents and
system components should be allowed to write local observations or raw
learnings as they encounter them: authors, reviewers, reporters, merge
logic, telemetry, and supervising agents. Dedicated `learn` tasks then
curate that stream: verify it, improve wording, compact duplicates,
remove stale guidance, connect learnings across work items/attempts,
inspect full transcripts across contexts, and decide whether project
local expertise should be updated or an upstream/distributed Factory
work item should be proposed.

Observations and learnings have different default paths. Observations
feed intake because they are candidate work, incidents, questions,
opportunities, or signals that may need triage. Learnings improve future
agent behavior and do not necessarily feed intake, though a learning may
generate an observation when it implies work should happen. Agents may
write directly to project-local `.factory/expertise/*`; periodic curation
by `learn` tasks is enough for local expertise quality. Human review is
optional for local expertise, and humans can add observations when they
notice disagreement, drift, or a need for stronger review.

All task kinds may read project-local `.factory/expertise` by default.
Prompts should label it as local learned guidance, not authoritative
source documentation. A useful authority framing is: work item artifacts,
explicit user instructions, source docs, and durable behavior docs are
authoritative; `.factory/expertise` is advisory local guidance;
observations, transcripts, and telemetry are raw evidence. Conversation
agents may edit project-local `.factory/expertise` directly when
collaborating with the user outside delegated run execution. Sandboxed
tasks should instead write learning artifacts into their task artifact
area; Factory runner/reporting/land/learn machinery can ingest, copy,
merge, or curate those artifacts into project-local `.factory/expertise`
without giving delegated tasks broad write access to shared Factory
state. This avoids service-to-service machinery while preserving sandbox
boundaries and attribution to the task, run, or work item that produced
the learning.

Project-local durable Factory memory should be committed to the managed
project repository by default. That includes `.factory/observations.md`
and `.factory/expertise/*`. Git should be the revision system for this
memory rather than a separate expertise revision marker: tasks can record
the commit/worktree state they read, changes are diffable and reversible,
and humans can review memory updates through normal project history when
they want to. Keep ephemeral Factory runtime state separate: runs,
transcripts, usage snapshots, locks, active pointers, and other runtime
artifacts should remain outside the durable committed memory model unless
explicitly promoted.

Future expertise layering may include mounted external knowledge sources:
company-level context, team-shared expertise, user-local expertise, or
other directories mounted into Factory-managed projects. Keep the current
default as committed project-local `.factory/expertise`, but design the
context loader so advisory expertise can eventually come from multiple
readable sources with clear precedence and authority labels. Mounted
sources should be read-only by default unless Factory has an explicit
owner and write policy for that source; project-local `.factory/expertise`
remains the normal learned local write target.

2026-06-06 — Full-codebase review run `20260606-161051` produced a set
of findings that are all worth fixing and should not be lost when review
worktrees are cleaned. Treat them as a backlog, not one monolithic patch:

- Remove the legacy `scripts/factory` shell implementation and stop using
  it as the Fargate task runtime. Fargate should share the Rust session
  lifecycle rather than routing through a separate shell implementation.
  → Landed in `557b6a2`, `dd5f361`, and `453f01c`.
- Add Fargate success-path coverage for launch command construction,
  workspace upload/download, task status, `factory pull`, and
  `factory shell`; current coverage is mostly negative paths.
  → Launch, entrypoint, pull, shell, and metadata preservation coverage
  landed in `dd5f361` and `453f01c`.
- Fix the dashboard behavior regressions in `test-dashboard.sh`: live
  run status in tabs, initial active-run selection, and polling after
  source run deletion.
  → Verified on `main`: `test-dashboard.sh` passes all 12 cases.
- Split or clarify the dashboard module boundary so filesystem state,
  transcript loading, event handling, rendering, and tests are easier to
  change independently.
- Move review verdict interpretation out of the durable run-state model
  and into the review subsystem, or otherwise define a clear run/review
  boundary.
  → Resolved: `review.rs` owns review acceptance interpretation from
  `review-state.json` and legacy current-round review artifacts; `run.rs`
  delegates to the review subsystem after resolving source/live artifact
  directories, and architecture docs capture the boundary.
- Centralize the "prefer live worktree run state over source run state"
  rule so status, summary, dashboard, resume, and land agree.
  → Landed in `a861477` and `1a13dba`.
- Make reviewer launch failures operationally visible instead of
  collapsing missing/failed reviewers into pass-like behavior.
  → Landed in `5989e94`.
- Decide whether skills and expertise are filesystem-only or part of the
  `ContentResolver` project/user/bundled chain, then align docs and code.
  → Resolved in `20260607-001118`: `ContentResolver` is documented and
  tested as runtime content resolution for prompts and sandbox profiles
  only; skills and expertise remain filesystem/agent-managed content
  outside the project/user/bundled resolver chain.
- Update architecture docs for active modules (`checks`, `config`,
  `cleanup`, `land`) and document model-selection environment variables.
  → Resolved: architecture now documents active module responsibilities
  for `config.rs`, `checks.rs`, `land.rs`, `cleanup.rs`, and `coder.rs`,
  including `FACTORY_CLAUDE_MODEL`, `FACTORY_MODEL`,
  `FACTORY_CODEX_MODEL`, `FACTORY_CODER`, and
  `FACTORY_CODEX_CA_BUNDLE`; behavior scripts assert those docs remain.
- Improve planning-skill behavior coverage, especially
  `define-behaviors` producing `behaviors.diff.md` and the mismatch
  where the text-only skill harness is credited with testing codebase
  research even though it disables file/tool access.
  → Resolved: added behavior, approach, and planning scenarios for
  project checks; `tests/test-skill` now stores planning artifacts under
  `behaviors.diff.md`, `approach.md`, and `plan.md`; the behavior map now
  labels text-only harness coverage as conversation/artifact coverage,
  not real codebase research.
- Remove or relabel `tests/test-run` shell-function coverage so the
  behavior map reflects the Rust binary that actually ships.
  → Landed in `557b6a2`.

2026-06-06 — Review-limit completion and land currently disagree. Run
`20260606-180035` reached the maximum review rounds, fixed the latest
blocking architecture finding, committed a clean worktree, and the
session loop accepted the run as complete. `factory land` still rejected
it because the top-level `review-architecture.md` artifact from the
previous review round still had verdict `fail`. Recovery archived that
stale review artifact before landing. Factory should make this contract
explicit: either review-limit completion must rerun or clear stale
top-level verdicts before completing, or `land` must understand an
accepted review-limit completion marker. The source of truth for review
verdicts should live in the review subsystem, not leak as ambiguous
durable run state. In addition to tightening that contract, Factory may
need to raise or tune the review-round limit so useful runs do not hit
the ceiling while they are still making productive progress.
surface where any observing human can act on a cue. That likely needs a
permission model over time, so different humans can be allowed to
observe, triage, approve runs, restart runs, resolve needs-user items,
or land changes at different levels.

Learning capture should happen at every level of this system, not only
as an after-the-fact human note. Individual agents can record local
learnings from their session: codebase facts discovered, wrong
assumptions corrected, tool failures, review misunderstandings, and
what they would do differently. A run-level observer or reporting agent
can synthesize learnings across author and reviewer sessions: why the
run looped, which artifacts were missing, which tests or environments
behaved differently, and what should change in Factory process. Across
runs, the land command or merge queue can detect recurring patterns and
turn them into durable observations, expertise, behavior mappings,
checks, or decisions. Any time work bubbles back up to the human
operator or coordinating agent, that is itself a signal: Factory lacked
enough automation, context, policy, artifact quality, or recovery logic
to finish autonomously, and the event should be captured as input for
improving the system. The agent focused on learning capture should look
at full transcripts from multiple agents and, when synthesizing broader
patterns, multiple runs. Final reports and handoffs are useful summaries,
but full transcripts preserve false starts, reviewer/author
disagreements, tool failures, and repeated human interventions that can
disappear from polished artifacts. Learning synthesis should cite which
transcripts or runs informed it and distinguish single-run lessons from
cross-run patterns.

One review role or expertise file should also nudge changes toward
vocabulary consistency. This may belong in architecture expertise,
documentation expertise, or both: architecture can check whether a term
matches the domain model and component boundaries, while documentation
can check whether user-facing names stay consistent across behaviors,
docs, tests, commands, and dashboard copy. The design question is how to
make this a gentle review signal rather than churn over harmless wording.

2026-06-06 — Parallel parent recovery needs to be merge-phase aware. Run
`20260606-queues-cleanup-reporting` produced useful child commits, but
the parent failed during child landing because the source worktree had
new dirty observation edits. After the observation was committed, a
parent resume restarted the child plan instead of resuming only the
failed merge/land phase. That reset child metadata, damaged the `1-1`
branch pointer, and then failed all relaunched children under nested
`sandbox-exec` with `sandbox_apply: Operation not permitted`. Factory
should prevent dirty source worktrees before parent landing, preserve
completed child state, support merge-only recovery for failed parallel
parents, and avoid relaunching completed child work when the only failed
step is parent-side merge/land.

2026-06-06 — Clarify the boundary between conversation-state edits and
delegated run execution. The agent that is actively collaborating with a
user should be allowed to write discussion artifacts directly: briefs,
observations, behavior drafts, approaches, plans, and lightweight
curation. That keeps the human planning loop fast and avoids pushing
work that can be done directly into unnecessary runs. The same agent
should not meddle with live run state: run branches, worktrees, statuses,
session artifacts, child metadata, and landing state belong to the run
system unless the user explicitly approves recovery. To keep `main`
available as a stable rebase and merge target, direct conversation edits
should happen on a lightweight discussion branch or worktree whenever
active runs or parent landing could overlap with those edits.

2026-06-07 — Factory needs a workflow to start a conversation-focused
Codex coordinator with the right operational permissions up front. The
coordinating instance should be able to create Factory runs, resume
them, install rebuilt binaries, and perform normal local orchestration
without repeatedly asking the human for permission after the initial
trust decision. This is distinct from loosening permissions for delegated
run agents: delegated runs should still execute inside their intended
sandbox/runtime boundaries. The missing workflow is a trusted,
conversation-facing launcher or profile for the human-agent planning
loop, so the coordinator can use Factory effectively while run execution
remains isolated.

2026-06-06 — General concurrency should not require a parent run.
Factory currently models most parallel work as one parent plan that
spawns child runs and owns the group merge. That is useful for
decomposing a single large brief into dependent or synthesized pieces,
but it is the wrong default for five unrelated observations or tasks.
Factory should support many independent active runs as peers in the run
queue, dashboard, and merge queue. Parent/child runs should represent
work decomposition and dependency structure, not general scheduling.
Independent runs need dependency metadata only when one run must start
or land after another; otherwise they should execute and land
independently so one parent failure cannot tangle unrelated work.
The PDF/YouTube expertise work moved to a separate conversation thread
because Factory does not yet let the coordinating agent trigger several
independent peer runs in parallel from one planning conversation. That is
a workflow smell: separate chat threads are being used as a substitute
for a first-class independent run queue.

2026-06-07 — Factory scheduling should use Codex `/status` as the live
subscription-capacity signal when scheduling Codex-backed runs. The
current Codex `/status` view exposes exactly the live fields Factory
needs: active model, account/plan, context-window remaining, 5-hour
limit remaining with reset time, weekly limit remaining with reset time,
GPT-5.3-Codex-Spark-specific limits, and a stale-warning signal. This is
different from the documented Codex Enterprise Analytics API: the
analytics API is useful for historical, delayed, workspace-level usage
and calibration, but it is not the right live scheduler signal for a
personal or Pro-style 5-hour/weekly subscription window. Factory should
add a Codex usage probe abstraction that tries to obtain `/status`
non-interactively, parses the usage fields, and stores a snapshot such as
`.factory/usage/codex-status.json` for the scheduler and dashboard. If
Codex exposes `/status` through `codex exec --json` or another supported
programmatic surface, use that. If it is TUI-only, evaluate a small
PTY-based probe or manual status import; avoid scraping the web
dashboard. Factory should also maintain its own local usage ledger from
Codex JSON `turn.completed.usage` events as fallback and calibration.
Scheduling should combine live `/status` remaining/reset data with
run-cost estimates so the run queue can burst when the 5-hour window is
healthy, preserve weekly budget when pacing is low, and switch to
planning/curation/reporting work when Codex capacity is scarce.

2026-06-07 — New model adoption should be a breaking redesign, not a
permanent compatibility bridge. The target operational model is Work
Item, Attempt, Task, Workspace, and Merge Candidate. `.factory/runs`
should stop being a first-class execution model once the replacement
works; old run/session-loop code, run-centric docs, run-centric tests,
and run-centric dashboard concepts should be deleted rather than carried
indefinitely.

Adopt the new model in this sequence:

1. Define the target state in code and docs. Work Items hold durable
   intent and planning artifact versions. Attempts represent bounded
   tries or phases to satisfy a Work Item. Tasks are schedulable units:
   `write`, `review`, `merge`, `report`, `learn`, and `probe`.
   Workspaces are Factory-managed filesystem/git contexts. Merge
   Candidates are reviewed results waiting to land.
2. Extend durable storage under `.factory/work/` beyond
   `.factory/work/items/`. Avoid making one nested Work Item JSON file
   carry all live operational state once tasks are running. Store live
   objects in separate collections, with references between Work Items,
   Attempts, Tasks, Workspaces, and Merge Candidates.
3. Replace run creation with Work Item and Attempt operations. Add the
   missing transition from `WorkItem -> Attempt -> initial write Task`.
   Existing command names may stay only if they map fully to the new
   concepts; otherwise prefer explicit `work`, `attempt`, `task`, and
   merge-candidate commands.
4. Implement task execution. Start with `write` tasks: allocate a
   workspace, run the selected coder, require clean committed output
   before task completion, and record produced commits/artifacts. Then
   implement `review` tasks as read-only candidate-workspace tasks that
   write only task artifacts and create follow-up `write` tasks for
   concrete fixes.
5. Implement the Attempt loop. An Attempt creates an initial write task,
   runs review tasks from explicit review policy, creates follow-up write
   tasks for failed reviews, moves uncertain review output to
   `needs-user`, and creates a Merge Candidate only after review passes.
6. Implement the Merge queue. Merge Candidates become the only path to
   `main`: rebase, run checks, run the full required reviewer set,
   fast-forward land, record reporting/learning artifacts, and clean
   workspaces.
7. Update dashboard/status around Work Items, Attempts, Tasks,
   Workspaces, Review artifacts, Merge Candidates, and Needs-user items.
   The first adoption slice should preserve legacy Runs while exposing
   Work state; a later breaking slice can remove the old Runs view or
   replace it with an Attempts-oriented view.
8. Rewrite skills and documentation to use the new vocabulary. Briefs,
   behaviors, approaches, and plans attach to Work Items and Attempts.
   Execution happens through Tasks. Landing happens through Merge
   Candidates.
9. Delete the old model after the new execution/review/merge path works:
   remove `.factory/runs` readers/writers, legacy run/session-loop code,
   old run tests, run-centric docs, and compatibility language.
10. Iterate from the new base: independent task scheduling,
   Codex/Claude capacity planning, Fargate task execution, learning
   capture, dashboard interventions, and team permissions.

Progress:
- `407ca59` added the first operational transition:
  `factory work attempt <work-item-id> <attempt-id>` appends a planned
  Attempt plus initial `write` Task from an existing Work Item.
- `73b01db` added write Task execution with the clean committed
  workspace invariant.
- `d699981` and `0ec8788` added review Task planning/execution,
  read-only candidate review enforcement, review artifacts, and stale
  review artifact protection.
- `2cba3a2` and `afb28cf` added the Attempt loop. It drives one Attempt
  through write Task execution, review Task planning/execution,
  follow-up write Tasks for failed reviews, `needs-user` handoffs for
  uncertain or missing verdicts, and stops at the Merge Candidate
  boundary.
- `fc5b54a`, `208dde2`, and `4862b23` added Merge Candidate creation.
  Passed Attempt reviews now create or return one durable Merge Candidate,
  candidates record source/target workspace and branch provenance, the
  Work model enforces one candidate per passed Attempt, and users can
  inspect candidates with `factory work merge-candidate`.
- `9852155` added Merge Candidate execution through `factory work merge`.
  Merge execution now validates candidate provenance and clean workspaces,
  rebases against the target branch, runs configured checks, runs the full
  merge-time reviewer set, fast-forwards the target branch, records
  durable merge status and artifacts, and cleans managed candidate
  workspaces after landing.
- `1630e30`, `11fa927`, `25cb457`, `a80d021`, and `605475d` added Work
  status/dashboard visibility. `factory status` now shows Work Items
  beside legacy Runs, and the dashboard has a Work Items view with
  Attempts, selected Tasks, Merge Candidates, merge state, needs-user
  state, read errors, polling refresh, and actionable/error counts.
- `8ebf4b2` updated the build workflow skills and architecture/behavior
  documentation to teach Work Items, Attempts, Tasks, Workspaces, and
  Merge Candidates as the target lifecycle, while keeping legacy
  `.factory/runs` commands documented as a transitional fallback.

The next adoption slices should enforce the new task/review boundaries
in normal workflow; clean up Work workspaces and artifacts; and then
delete legacy `.factory/runs` compatibility once the Work execution path
is used end to end.

2026-06-07 — Authors are increasingly using expertise, especially when
the approach lists specific expertise files, but Factory should make this
more explicit and auditable. A good next improvement would be: every run
report records “expertise loaded” based on transcript evidence, and
reviewers can flag when the approach names expertise but the author never
read it.

2026-06-08 — Factory should teach agents to turn internal behavior and
architecture artifacts into more human-readable, polished public
documentation. Current behavior docs and `behaviors.diff.md` files are
valuable as precise contracts, but they often read like internal test
scaffolding: dense EARS statements, implementation nouns, and long test
reference lists. That is useful for reviewers and automation, but it is
not the same as documentation that helps a human understand the product.

The skills and expertise should make this split explicit:
- `define-behaviors` should continue producing precise, testable
  behavior contracts.
- `write-documentation` should translate those contracts into concise
  user-facing prose, grouping related behaviors into readable workflows
  and explaining the user-visible meaning instead of mirroring every
  contract statement.
- `review-documentation` should check not only accuracy and coverage but
  also whether public-facing docs read like polished documentation rather
  than a restated behavior test matrix.
- Writing expertise should give concrete examples of converting EARS
  statements and architecture notes into public docs while preserving
  vocabulary consistency and test traceability.
