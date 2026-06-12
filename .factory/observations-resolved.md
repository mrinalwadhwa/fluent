# Resolved Observations

Observations that have been acted on. Kept for potential pattern
analysis later.

---

2026-06-10 — `factory cleanup --apply` should not delete a Work
Item whose only Attempt is `failed` due to a rate-limit error. The
current cleanup logic treats any non-running Attempt as terminal
and removes the whole Work Item including its durable planning
context.
→ Resolved: `27c8fbd` made rate-limit responses trigger retry
inside `Coder::run` rather than propagating as Task failure, so a
rate-limited Attempt no longer ends up `failed`. The cleanup
behavior is correct under that contract; the precondition that
made this observation relevant no longer holds.

2026-06-10 — `FACTORY_MAX_PARALLEL_REVIEWERS` env var is read but
not enforced. The `parallel-attempt-reviewers` Work Item's tests
reviewer flagged this as an advisory finding: the cap value is read
into `_cap` (underscore prefix) and never used, so all planned
review Tasks always spawn unconditionally.
→ Resolved: `915eb3c` (landed directly via fast-forward merge per
the Factory-too-slow override) replaced the `_cap` read with a
`Mutex<usize> + Condvar` semaphore guarded by an RAII `SlotGuard`,
serialized Work Item store access during parallel review with a
`store_lock` mutex, kept review-only Attempts serial, and added a
`cap_enforcement_limits_in_flight_reviewers` test that asserts peak
in-flight count never exceeds the cap.

2026-06-10 — Merge-time review failure should not require the
conversation agent to intervene. Today when a Merge Candidate's
merge-time reviewers return `fail`, the Merge Candidate transitions
to `failed` and the lifecycle stops. The conversation agent then has
to draft a new Work Item that cherry-picks the prior candidate
commits and applies fixes for the merge-time findings, then runs
that new Work Item from scratch.
→ Resolved: `4949b04` added `MAX_MERGE_FOLLOWUP_WRITES_PER_INVOCATION`
(2) and refactored `execute_merge` into a loop. On merge-time review
failure with budget remaining, Factory now invokes a follow-up writer
against the candidate workspace with the failed review artifacts as
input, then restarts the rebase + checks + reviews cycle. Budget
exhaustion produces a `needs-user` Merge Candidate state plus a
handoff naming the failed review paths.

2026-06-10 — A coder rate-limit response should not become a hard
Task failure. When Claude returned `You've hit your session limit · resets 7:10pm`,
Factory recorded the exit code as a Task failure and marked the
Attempt `failed`. The conversation agent then had to cleanup,
re-create the Work Item, and re-run.
→ Resolved: `27c8fbd` added `transcript_indicates_rate_limit` and
`run_with_transcript_retrying`. Coder runs whose transcripts contain
session-limit or rate-limit markers now sleep
`FACTORY_RATE_LIMIT_RETRY_AFTER_SECS` (default 1800) and retry up to
2 times before propagating the exit code. Author and reviewer Tasks
inherit the retry without a Coder trait surface change.

2026-06-09 — Some behavior shell tests assume the repository's default
`target/debug/factory` build output. During merge review for
`work-planning-bridge-cleanup`, `test-work-task-instructions.sh` was
awkward to run from a read-only candidate workspace because merge
reviewers are supposed to redirect Cargo output into artifact-local
directories. Behavior scripts should consistently support an explicit
Factory binary path or artifact-local build output so reviewers can run
them without writing into candidate workspaces.
→ Resolved: `1f69ab3` added `FACTORY_BIN_OVERRIDE` plumbing to selected
Work behavior scripts and added a mock-binary operation test. `8f69a10`
extended override coverage to operation scripts more broadly and aligned
behavior docs with the suite-wide contract. `90190f6` tightened
override behavior for `test-run-curation.sh` and confirmed the override
test surface. `test-work-task-instructions.sh` and the rest of the
behavior suite now read `FACTORY_BIN_OVERRIDE` with a `target/debug/factory`
default, so reviewers can point them at the artifact-local candidate
build.

2026-06-09 — The first attempt to run independent peer Work Items in
parallel exposed a Work artifact namespace bug. Two Work Items
(`cleanup-empty-work-artifact-dirs` and `build-skill-work-default`) both
used `attempt-1` and review task ids such as
`attempt-1-review-documentation`, so reviewers from both items wrote to
the same `.factory/work/artifacts/attempt-1/...` paths. One review
artifact was overwritten with findings for the other Work Item. The
author commits were intact on separate branches, but review state was
not trustworthy. Before using peer Work Items in parallel, Work artifact
paths need to include the Work Item id, or another globally unique run
namespace, so attempt/task ids only need to be unique within a Work Item.
→ Resolved: `af7d61f` added `work_artifact_path(work_item_id, attempt_id,
artifact)` and routed task, attempt, and merge artifact construction
through it so new artifacts live under
`.factory/work/artifacts/<work-item-id>/<attempt-id>/<artifact>`.
`WorkModelStore` normalizes legacy `attempt-only` paths at the storage
boundary on read, and `1088f6e` documented the migration. Tests
`review_artifact_paths_include_work_item_namespace` and
`store_migrates_legacy_work_artifact_paths_on_read` lock in the new
layout and the migration.

2026-06-09 — Work write Task prompts still carry a legacy run status-file
contract. During `work-planning-bridge-cleanup`, a Work follow-up author
was told to write `.factory/runs/[run-id]/status`, and the candidate
ended up with `.factory/runs/attempt-1-write/status = complete`.
Work write Tasks should be Work-native: task completion should mean a
clean committed workspace plus durable Task/Attempt state, not delegated
authors writing legacy run status files.
→ Resolved: `42577d2` clarified the Work write Task no-change
completion prompt, and `3b9d0aa` added `prompts/work-author.md` plus
Work task executor wiring so Work write Tasks no longer receive the
legacy run status/handoff author prompt. Focused Rust and shell behavior
tests now assert that Work write prompts mention the Factory Work model,
warn that no committed Task output fails, and exclude legacy
`.factory/runs` status and `handoff.md` instructions.

2026-06-09 — New model adoption still had reviewer prompts and merge
review prompts that spoke in legacy `.factory/runs` terms even when
Factory was executing Work review Tasks and Merge Candidate reviews.
→ Resolved: `201e8a5` added Work-native `[work-system]` sections to the
bundled reviewer prompts, taught Work review Task prompts to name Work
artifact paths and artifact-local writable output locations, taught
merge-time reviewer prompts to prefer `[work-system]` with legacy
fallback, and documented the Work review prompt contract in architecture
and behavior docs.

2026-06-09 — Work model storage still had a compatibility bridge where
`.factory/work/items/<id>.json` could contain nested Attempts, Tasks, and
Merge Candidates when no split records existed. The adoption plan called
for live Work objects to move into separate durable collections instead
of carrying one nested Work Item JSON file indefinitely.
→ Resolved: `4f9c52f` and `bc2c4e6` made split Work storage
authoritative. `WorkModelStore` now parses item files as Work Item
metadata, assembles Attempts, Tasks, and Merge Candidates from
`.factory/work/attempts/`, `.factory/work/tasks/`, and
`.factory/work/merge-candidates/`, ignores nested operational collections
in item JSON, updates storage documentation and behavior contracts, and
adds focused storage, CLI, behavior, and external-review tests.

2026-06-09 — Small Work Items were taking avoidable follow-up loops
because the initial author prompt did not explicitly ask the author to
preflight likely touched behavior statements, user-facing docs, tests,
skills/expertise, and verification commands before editing.
→ Resolved: `36d244c`, `2ac1414`, and `a2694ea` added Work write Task
author preflight guidance, follow-up input-artifact guidance, behavior
contract documentation, binary prompt assertions, and operation behavior
coverage. This resolves the first speed-up slice; the broader latency
measurement and merge/review scheduling observations remain open.

2026-06-08 — Work Attempt follow-up loops reran the full reviewer set
after every small follow-up writer Task. That preserved quality, but it
made review loops slow when only one reviewer finding changed. The first
slice should keep the full required reviewer set as the merge-queue
safety gate while narrowing intermediate Attempt review rounds to the
failed reviewer roles that fed the follow-up write Task, with a
conservative fallback to the full reviewer set when provenance cannot be
derived.
→ Resolved: `66db98c` and `b895a08` added targeted follow-up review
planning in the Work Attempt loop, deriving roles from completed review
Task producer ids in follow-up `input_artifacts`, falling back to the
full reviewer set when mappings are missing, preserving full initial and
merge-time reviews, updating architecture/behavior docs, and adding
unit, binary, and operation behavior coverage.

2026-06-08 — Work-model adoption needed approved planning artifacts to
live directly on Work Items instead of being flattened into a legacy
`.factory/runs/<run-id>/execution-instructions.md` bridge before
delegated execution.
→ Resolved: `4ade899` and `e2b5a5d` added first-class Work planning
context, CLI flags for separate brief/behaviors/approach/plan files and
combined planning context, initial and follow-up write Task prompt
derivation from durable Work state, build/planning skill updates,
architecture and behavior documentation, and focused binary coverage for
initial write Tasks, task prompt propagation, precedence, and failed
review follow-up Tasks.

2026-06-08 — New model adoption needed operator visibility for Work
Items, Attempts, Tasks, Merge Candidates, merge state, read errors, and
needs-user/actionable state. `factory status` and `factory dashboard`
still centered legacy Runs, so the Work model required manual JSON
inspection.
→ Resolved: `1630e30`, `11fa927`, `25cb457`, `a80d021`, and `605475d`
added `work_status.rs`, Work Item output in `factory status`, a dashboard
Work Items view, polling refresh, actionable/error counts, invalid Work
Item read-error reporting, needs-user visibility, architecture and
behavior docs, and behavior/binary/unit coverage.

2026-06-08 — New model adoption needed merge queue execution after
passed Attempt reviews created durable Merge Candidates. Merge Candidates
should become the path to `main`: validate provenance, rebase/update the
candidate, run configured checks, run required merge-time reviewers,
fast-forward land, record merge state and artifacts, and clean managed
workspaces.
→ Resolved: `9852155` added `factory work merge <work-item-id>
<merge-candidate-id>`, durable Merge Candidate merge state, merge-time
check and review artifacts, idempotent already-landed handling, rebase
and target-move protection, failure recording, workspace cleanup, and
behavior/binary/model coverage.

2026-06-08 — New model adoption needed Merge Candidate creation after
Attempt reviews passed. A passed Attempt should create or return one
durable candidate result, record the reviewed source workspace, target
workspace, branch provenance, and candidate commit, expose candidate
inspection through the Work CLI, and still stop before merge queue
execution.
→ Resolved: `fc5b54a`, `208dde2`, and `4862b23` added durable
`MergeCandidate` storage on Work Items, `factory work merge-candidate`
inspection, Attempt-loop candidate creation after passed reviews,
idempotent reruns, one-candidate-per-Attempt validation, documentation,
and behavior/binary/model coverage. The remaining new-model adoption work
starts at merge queue execution.

2026-06-07 — New model adoption needed an Attempt loop after Work Item,
Attempt, write Task, and review Task primitives existed. The loop needed
to drive one Attempt through planned write/review Tasks, create
follow-up write Tasks from failed review artifacts, move uncertain or
missing verdicts to `needs-user`, and stop before Merge Candidate
creation.
→ Resolved: `2cba3a2` and `afb28cf` added
`factory work attempt run <work-item-id> <attempt-id>`, review verdict
interpretation for Attempt rounds, follow-up write Task creation with
usable input artifacts, managed review artifact path validation,
`needs-user` handoffs, documentation, and behavior/binary coverage. The
remaining new-model adoption work starts at Merge Candidate creation and
merge queue execution.

2026-05-16 — Interactive planning skills still need more scenario
coverage. `capture-brief` has multiple scenarios, and
`define-behaviors` now has an initial run-summary scenario. The remaining
gap is focused coverage for `design-approach` and `plan-execution`, plus
deeper define-behaviors cases that verify final artifact quality instead
of only conversation structure. These skills drive the planning phase, so
scenario tests should simulate the interview flow and verify outputs.
→ Resolved: added `format-check-behaviors`, `format-check-approach`, and
`format-check-plan` scenarios, updated the behavior coverage map, and
taught `tests/test-skill` to write planning skill artifacts as
`behaviors.diff.md`, `approach.md`, and `plan.md` instead of always
using `brief.md`.

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

2026-05-12 — Author agent had the same working directory bug as
reviewers — running from main/ instead of the worktree.
→ Resolved: cd to worktree in cmd_run_bare and cmd_run_local before
run_session_loop. Also disable commit.gpgsign in worktree git config
so agents can commit without hardware key interaction.

2026-05-09 — Building the factory itself doesn't use factory run.
→ Resolved: first successful self-build run (test-coverage-20260512).
The factory built its own test coverage — 16 tests across 2 files,
all 5 reviewers passed, single session completion.

2026-05-13 — Reviewers have no timeout. A stuck reviewer ran for hours
blocking the entire review phase.
→ Resolved: added 30-minute timeout to run_single_reviewer. Reviewer
process is killed if it exceeds the timeout, verdict defaults to pass.
REVIEWER_TIMEOUT env var overrides the default. Rust version needs the
same timeout.

2026-05-12 — setup_run_worktree reuses an existing branch at its old
commit instead of current HEAD, causing stale code on retries.
→ Resolved: when branch exists, reset it to current HEAD with
git branch -f before checking out. Fixed in both shell script and
Rust binary. Test added. First run completed using the Rust binary.

2026-05-13 — macOS notifications fire when run status changes but
have no useful content — you know something happened but not what.
→ Resolved: 31bf063 (notification now includes run ID, status, brief
summary, session count, review verdict, and handoff open questions)

2026-05-12 — The define-behaviors skill should read existing behaviors
from documentation/behaviors.md before writing new ones. This would
calibrate the level of behavioral definition and avoid duplicating
behaviors that already exist.
→ Resolved: 1237508 (define-behaviors reads documentation/behaviors.md
and writes behaviors.diff.md as an increment over existing behavior)

2026-06-05 — The plan phase identifies parallelizable steps but the
factory has no mechanism to execute them in parallel. The factory should
support decomposing a plan into parallel child runs, launch them
simultaneously, and gate later work on completion.
→ Resolved: e49d797, 9d62538, 2014fff, 992930e (structured parallel
plans create child runs, launch parallel groups, gate sequential groups,
and land completed child branches)

2026-06-05 — When a run completes, the dashboard should show the run's
report (report.md) in the activity feed or a dedicated pane. The report
summarizes what happened across all sessions and review rounds.
→ Resolved: df6bdb9, 014ade6, 8291d76 (dashboard shows report.md by
default for completed runs and keeps transcript tabs accessible)

2026-06-05 — The dashboard never removes runs that were deleted from
disk. App::poll discovers new runs but never prunes stale ones, leaving
removed runs in the list with "[-]" status.
→ Resolved: 1fc4b8c (dashboard polling removes deleted source runs and
selects an existing run or the empty state)

2026-06-05 — The run tab shows "[planned]" for runs that are actively
executing because the tab reads source run status instead of live
worktree status.
→ Resolved: 1fc4b8c (run tabs use cached live status from the same
live_dir source as the header)

2026-06-05 — Codex sandbox support needs a focused verification run.
The implementation should verify Codex auth/config access, JSON
transcript output, worktree-limited writes, no sibling writes, and
credential handling under the Factory Seatbelt wrapper.
→ Resolved: 77aeddd, 11d0313, d50b2c3 (Codex runs inside the Factory
Seatbelt profile, uses a Codex-specific profile layer, disables Codex's
inner sandbox under Factory control, and receives a file-based CA bundle
when needed)

2026-06-05 — The dashboard can show inconsistent state while a run is
being fixed after review. The header showed the selected run as
`executing` while the tab showed `[planned]`.
→ Resolved: 1fc4b8c (dashboard uses live run status consistently for
header selection and run tabs)

2026-06-05 — Formatter churn should be prevented by process, not cleaned
up after the fact. Factory should run the repo's formatter consistently
before merge so formatting diffs are deliberate and reviewer-visible.
→ Resolved: 42531ff (Factory supports configurable pre-land checks with
autofix commands)

2026-06-05 — Run `20260605-193223` addressed the dashboard stale-status
and deleted-run observations: run tabs now use the same cached live
status as the header, initial selection prefers live active runs, polling
removes source run directories that disappeared, and the dashboard falls
back to an existing run or the empty state when the selected run is
removed.
→ Resolved: 1fc4b8c, c83bf1b (implementation and behavior coverage
landed for live dashboard run state refresh)

2026-06-05 — Add a `factory version` command that prints the installed
binary version plus the Git commit ID it was built from so users can
confirm which source commit the active binary corresponds to.
→ Resolved: fc81453, 1a696f5 (factory version prints package version and
build metadata and has behavior coverage)

2026-06-05 — Local run filesystem sandboxing should allow exactly the
run worktree plus the source repository's common git directory, not the
entire workspace parent. The sandbox should let agents commit from linked
worktrees without exposing unrelated sibling worktrees.
→ Resolved: bf2f323, 77aeddd, 11d0313 (local sandbox roots were narrowed
and Codex/Claude sandbox profiles render coder-specific writable roots)

2026-06-06 — Stale run artifacts need a first-class cleanup policy rather
than manual deletion. Cleanup should happen where the Factory state
resides: the source worktree's `.factory/runs` registry and its
registered git worktrees. It should not be modeled as ordinary author
work inside an isolated run worktree, because that worktree only carries
its own copied run state. Landed and reported runs should remain
queryable but should not dominate the default dashboard view.
Complete and landed stale runs need a `factory cleanup` command that
preserves the cleanup reason in the source Factory state and removes
registered git worktrees safely. Superseded planned runs, failed smoke
runs, and other stale artifacts still need an explicit
abandoned/superseded status or archive marker outside the current
cleanup command scope.
The leftover Codex smoke worktrees (`20260605-codex-installed-smoke`,
`20260606-codex-installed-ca-smoke`, and
`20260606-codex-installed-seatbelt-smoke`) point at commits already
contained in `main`, but the curation run could not remove their sibling
worktree directories because Git could not validate those paths under
the run sandbox's filesystem permissions.
→ Resolved: `factory cleanup` preserves run directories, writes
`cleaned.md` for complete and landed runs, removes only registered git
worktrees, skips unregistered paths, and keeps cleaned runs behind
actionable dashboard runs.

2026-06-05 — Dashboard "reviewing" status shows no spinner in the
header. compute_phase needs to map "reviewing" to animated=true.
Also, reviewer tabs show stale verdicts from the previous round
instead of resetting to "running" when a new review round starts.
The dashboard needs to detect that review artifacts have been
archived (moved to round-N/) and reset reviewer status accordingly.
→ Resolved: 04b083a, 307c112, a6b8f8a, bae62ca, 5a46c92 (dashboard
tracks the current review round, refreshes reviewer transcript state
for the active round, and has deterministic behavior coverage)

2026-06-05 — Factory review detection is commit-based. During run
`20260605-193223`, an author wrote valid implementation changes and
marked the run complete, but left the worktree dirty. Factory compared
`main..HEAD`, saw no committed diff, skipped reviews, and produced a
no-code-changes report. The session loop should require or verify a clean
committed worktree before `complete`, or Factory should detect dirty
worktrees and fail/needs-user instead of skipping reviews.
→ Resolved: cfba7c3 (dirty worktrees count as changed so completed
author work cannot bypass review because it was not committed)

2026-06-06 — `factory resume` should support non-interactive automation
or provide a separate headless resume path. During run curation,
`factory resume 20260606-run-curation --coder codex` failed with
`stdin is not a terminal`, while `factory run --run-id
20260606-run-curation --coder codex` could continue the run. Automation
should not have to know that distinction, and a resume path should be
usable from scripts, agents, or other non-TTY orchestrators when the
intent is to restart the session loop rather than attach interactively.
→ Resolved: bd82a58, a2f8d84, e057ae7, c757421, 53077d6 (headless
resume restarts selected or implicit resumable runs, rejects parallel
parent runs, and documents the selection behavior)

2026-06-05 — The dashboard animation still feels sluggish despite
the 100ms render interval. The spinner needs to cycle faster to
feel responsive — consider 50-80ms or a different animation style
that communicates activity more clearly at lower frame rates.
→ Resolved: fff24a9 (dashboard render cadence now uses a 75ms interval
and the behavior documentation reflects the faster animation target)

2026-06-08 — Work task execution needed a durable place for the rich
brief, behavior expectations, approach, and plan that should guide coder
execution. Passing that material as extra CLI args to
`factory work attempt run` was the wrong boundary because extra args are
coder flags and Codex treats additional positional text as invalid prompt
input.
→ Resolved: 03051d8, 0790846, 79444f4 (`factory work create` accepts
inline or file-backed instructions, stores them on the Work Item,
copies them onto initial and follow-up write Tasks, includes non-empty
`Task.instructions` in write prompts, and preserves extra args as coder
options)

2026-06-05 — The factory should be able to visually observe terminal
UIs during testing. Launch the dashboard (or any TUI) in a tmux
session, capture the screen with tmux capture-pane, and evaluate
the rendered output. This enables autonomous agents to catch
visual bugs (missing animation, stale status, rendering glitches)
without a human looking at screenshots. This should be a skill —
distributable expertise on how to test terminal user interfaces
using tmux capture and VT100 rendering.
→ Resolved: added the `test-terminal-ui` skill, backed by
`expertise/terminal-ui.md`, to package in-process render testing and
tmux capture as a reusable workflow.

2026-06-05 — The dashboard should surface more activity beyond the
header phase label and active agent tabs. Add active run indicators
in the run tabs (spinner next to status), sort active runs and agents
first in their respective lists, and consider a global activity
indicator in the dashboard title bar when any run is active. The
dashboard should feel alive when work is happening and completely
still when everything is done.
→ Resolved: fff24a9, 145d75d, and follow-up dashboard title work. The
dashboard now renders faster, shows active run markers in run tabs,
keeps actionable runs sorted ahead of terminal runs during polling, and
shows a dashboard-title activity spinner when any run is active. Agent
tabs already show running status; active-agent reordering was left out to
preserve stable author/report/reviewer tab positions.

2026-06-07 — Update `build-in-the-factory` command reference to include
`summary`, `dashboard`, `land`, `init`, and `version`.
→ Resolved: 6490b93, d4fbe64, ac11509, 13401ff (skill command reference
now lists the current core Factory CLI commands, describes `resume` as
supporting paused or failed runs, and has behavior tests that compare
the skill command block against this checkout's Factory binary)

2026-06-07 — Fix skill review findings: `review-behaviors` should not
tell reviewers to read `plan.md` unless the allowed-read boundary
explicitly includes it, and `design-approach` should use
`references/...` for expertise files instead of direct `expertise/...`
paths.
→ Resolved: 6168a98, 2a95f3a (review-behaviors guidance now matches its
visibility boundary, design-approach uses skill-local expertise
references, the design-approach skill packages all references advertised
by its index, and focused behavior tests cover both contracts)

2026-06-07 — Rewrite skills and documentation to use the new Work-model
vocabulary. Briefs, behaviors, approaches, and plans should attach to
Work Items and Attempts; execution should happen through Tasks; landing
should happen through Merge Candidates. Legacy `.factory/runs` guidance
should remain only as a temporary bridge until the new execution path
works end to end.
→ Resolved: 8ebf4b2 (the build workflow skill now teaches Work Item →
Attempt → Task → Workspace → Merge Candidate as the target lifecycle,
related planning/review skills and architecture/behavior docs use the
new vocabulary, and focused behavior tests cover the Work guidance and
command reference)

2026-06-09 — Planning skills still treated legacy run files as the
normal handoff in places, even after Work Items gained durable planning
context. Capture, behavior definition, approach design, and execution
planning should distinguish active pre-Work-Item planning conversation
artifacts from durable Work Item planning context and use legacy
`.factory/runs` planning files only for fallback or recovery.
→ Resolved: c10bd34. Planning skills now describe approved planning
drafts as the pre-Work-Item handoff, `factory work create` stores those
drafts as durable Work Item planning context, and legacy run planning
files are documented as fallback or recovery state. Architecture and
behavior docs plus `test-planning-skills-work-context.sh` cover the
boundary.

2026-06-08 — Merge-time reviewers still need a stricter Work-native,
read-only contract. During the `work-planning-artifacts` merge candidate,
the merge-time behavior reviewer received legacy `.factory/runs/...`
instructions even though the Work merge artifact path was
`.factory/work/artifacts/...`, then created useful scratch behavior tests
and documentation edits inside the candidate workspace. The merge landed
only the committed candidate and cleanup removed the transient worktree,
so those scratch edits did not land. This reinforces the redesigned
model: merge-time reviews should write only review artifacts, prompts
should use Work-native paths, and useful scratch tests or suggested edits
should become follow-up write Tasks instead of candidate mutations.
→ Resolved: fc382c1, ee9b549, ea96319, 6d4fce1, and 2715773 made
merge-time reviewers Work-native and read-only at the merge boundary.
Reviewer prompts now use Work artifact paths, absolute candidate skill
and decision paths, and read-only candidate guidance. Merge execution now
detects staged, unstaged, untracked, and ignored candidate workspace
mutations after each reviewer, records failed merge review state before
landing, and keeps reviewers writing artifacts instead of changing the
candidate.

2026-06-09 — Work-model behavior reviews do not have a first-class
`behaviors.diff.md` artifact like legacy runs did. During targeted
follow-up review work, behavior reviewers had to infer new behaviors
from `documentation/behaviors.md`, the candidate diff, and Work Item
planning context. The Work review prompt or artifact model should make
the Work Item behavior increment explicit so behavior reviewers can stay
within their no-source-code boundary without guessing from docs.
→ Resolved: 56d8dae. Work review Tasks and merge-time behavior
reviewers now receive a "Work behavior review input" prompt section from
`WorkItem.planning_context.behaviors`, or an explicit message that no
Work behavior increment was provided. `review-behaviors` now treats
legacy `.factory/runs/[run-id]/behaviors.diff.md` as a legacy-only input
and tells Work-model reviewers to use the prompt context and exact Work
artifact path.

2026-06-09 — Work Merge Candidate landing can record a false failed
state after the target branch has already fast-forwarded if managed
workspace cleanup removes the candidate workspace before the merge
driver's final status check. In `author-preflight-guidance`, `main`
advanced to `a2694ea`, merge-time reviews passed, and the managed
worktree was gone, but the merge candidate recorded `review_state:
failed` and merge status `failed` because the final `git status` check
could not `chdir` into the removed candidate workspace. The merge
executor should record landed state before cleanup and avoid checking a
workspace after it removes it; cleanup failures should warn without
turning an already-landed candidate into a failed one.
→ Resolved: b4e577b. Work Merge Candidate execution now recovers a
stored landed result if a post-landing error occurs,
`record_candidate_failure` does not overwrite a landed candidate with a
stored landed commit, and rerunning an already-landed candidate reports
the stored commit without requiring the removed candidate workspace.
Focused unit and binary tests cover the recovery helper, landed-state
failure guard, cleanup warning, and rerun-after-cleanup behavior.

2026-06-08 — Work Attempt follow-up review Tasks should receive the
prior failed review artifacts that led to the follow-up write. Factory
already reran only the failed reviewer roles that fed a follow-up write
Task, while keeping the full reviewer set as the merge-queue safety gate,
but reviewers still had to rediscover the concrete prior findings.
→ Resolved: 2156c34 and 885c9de. Factory now maps a completed
follow-up write Task's failed review input artifacts back to reviewer
roles, attaches the role-matched artifact to each targeted follow-up
review Task, includes those artifact paths and read-first guidance in
review prompts, and grants sandboxed read access to the prior review
artifact directories. Behavior, architecture, and binary tests cover the
new review input flow while merge-time reviews still run the full
reviewer set.

2026-06-07 — `factory resume --no-sandbox ...` treated `--no-sandbox` as
an extra agent argument because `resume` only read the top-level
`factory --no-sandbox resume ...` flag. Recovery commands did not work as
expected.
→ Resolved: `Resume` clap variant now accepts `--no-sandbox` and
`--coder` as local flags, matching `Run`. Dispatch combines local and
top-level forms with local taking precedence. Tests cover local flags,
global flags, precedence, help output, and no-leak into extra args.

2026-06-11 — Add a Rust `factory fargate teardown` command that
replaces `infrastructure/teardown.sh`, the same way JIT bootstrap
replaced `infrastructure/setup.sh`. Two different workflows for
parallel concerns (setup vs teardown) is unnecessary surface; both
should live behind the binary. The teardown command should: remove
the CloudFormation stack, optionally clean ECR images and the S3
bucket, and clear `~/.config/factory/fargate.state.json` so the
next `--runtime fargate` invocation bootstraps fresh.
→ Resolved: `factory fargate teardown [--keep-ecr] [--keep-s3]`
implemented in `src/fargate_bootstrap.rs::teardown()` with CLI
dispatch from `src/main.rs`. `infrastructure/teardown.sh` deleted.
Behaviors, architecture docs, and tests updated.
