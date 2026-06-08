# Resolved Observations

Observations that have been acted on. Kept for potential pattern
analysis later.

---

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
