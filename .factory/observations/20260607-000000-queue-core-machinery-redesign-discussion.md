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

Run `20260608-131938-work-task-instructions` reinforced that the legacy
run loop still treats dirty completion as recoverable after review rather
than preventing it at the write-task boundary. The author added one
review-requested documentation line, marked the run complete while that
line was uncommitted, reviewers passed against the dirty workspace, and
the loop then restarted the author to commit the already-reviewed change
before the final review round. This recovery path worked, but it is noisy
and should not be the target Work model behavior: a write task should
produce clean committed output before any review task runs.

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
