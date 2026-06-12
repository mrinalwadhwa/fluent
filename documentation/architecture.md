# Architecture

Workflow and execution system for autonomous coding agents. Manages work
from intent capture through execution and review across multiple sessions.

## System overview

```
┌─────────────────────────────────────────────────┐
│  Skills                                         │
│  capture-brief, define-behaviors,               │
│  design-approach, plan-execution                │
│  review-documentation, review-behaviors,        │
│  review-architecture, review-skills,            │
│  review-tests, architect, write-documentation,   │
│  write-tests, test-terminal-ui                  │
│  Portable procedures any agent follows          │
├─────────────────────────────────────────────────┤
│  build-in-the-factory skill                     │
│  Teaches agents the full workflow               │
├─────────────────────────────────────────────────┤
│  Factory command                                │
│  factory work / status / dashboard / cleanup    │
│  factory run / review / summary / watch         │
│  factory resume / land / pull / shell           │
│  factory init / version                         │
│  Deterministic, operational                     │
└─────────────────────────────────────────────────┘
```

Skills describe procedures. They don't know about sandboxes, sessions,
or runtimes. The factory command handles the operational envelope:
sandbox setup, credential injection, session continuity, worktree
creation, and remote execution. The build-in-the-factory skill bridges
the two — an agent reads it and can drive the entire workflow.

## Workflow

```
Brief → Behaviors → Approach → Plan → Execute → Review → Land
(interactive)                         (autonomous)
```

Interactive stages happen in the agent's session with the user present.
The agent follows skills directly.

Autonomous stages don't need the user. The Work model is the primary
delegated execution path. Legacy run commands remain available for
compatibility, Fargate, and recovery.

## Core work model

Factory's primary execution lifecycle uses these durable nouns: Work
Item, Attempt, Task, Workspace, and Merge Candidate. This model is
documented and represented in Rust so scheduling, status, review, and
merge paths use the same vocabulary.

The Work model now has an execution bridge for real delegated work.
`factory work task run <work-item-id> <attempt-id> <task-id>` executes a
stored write or review Task through the selected coder, `factory work
attempt run <work-item-id> <attempt-id>` advances an Attempt through safe
write and review transitions, and `factory work merge <work-item-id>
<merge-candidate-id>` executes a stored Merge Candidate. Legacy
`.factory/runs` commands remain supported as legacy compatibility for
explicit fallback, Fargate-only execution, coordinated child-run
decomposition, and recovery of existing run state.

`factory work create <id> --title <title>` exposes the first Work Item
intake surface. It writes Work Item metadata under
`.factory/work/items/` and leaves Attempt, Task, and Merge Candidate
collections empty. It does not schedule work or mutate legacy run state.
Callers may attach approved planning context directly to the Work Item with
`--planning-context <text>`, `--planning-context-file <path>`, or
separate `--brief-file`, `--behaviors-file`, `--approach-file`, and
`--plan-file` inputs. Factory stores that context as optional
`WorkItem.planning_context` so `factory work show <id>` exposes the
brief, behaviors, approach, and plan that write Tasks use. Planning
skills treat this Work Item planning context as the normal handoff to
delegated Work execution; legacy `.factory/runs/<run-id>/` planning
files are fallback or recovery state for paths the Work model cannot yet
carry. Callers may also pass explicit prompt text with `--instructions <text>` or
`--instructions-file <path>`; Factory stores that text as optional
`WorkItem.instructions` and gives it precedence over derived planning
context when it creates write Task instructions. `factory work attempt
<work-item-id> <attempt-id>` creates the first operational transition
from intake: it appends a planned Attempt with one initial `write` Task.
The Task declares role `author`, copies explicit Work Item instructions
or derives instructions from Work Item planning context into optional
`Task.instructions`, and declares one writable workspace reference at
`../work-<work-item-id-byte-len>-<work-item-id>-<attempt-id>`.
`factory work task run` creates or reuses that writable workspace as a
sibling git worktree beside the source checkout, runs the coder there,
and completes the Task only after the workspace is clean and contains a
new commit produced after Factory bound the workspace for that Task run.
The bridge stores workspace paths relative to the source root for
portability, resolves them through the source checkout parent at
execution time, and rejects writable Task workspace paths outside the
expected managed sibling workspace before it creates or binds a
worktree.
`factory work review <work-item-id> <attempt-id>` appends planned
`review` Tasks for the default reviewer set after a completed write Task
exists. Each review Task reads the candidate workspace, carries review
context copied from the write output, and writes only under
`.factory/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`. The
review context names the candidate workspace, source branch, and
candidate commit and includes a shell-quoted `git -C <workspace> diff
<range>` command so a reviewer can inspect the scoped diff without
rediscovering the author Task. Running a review Task requires
`review.md` in that artifact area; the Task can complete even when that
artifact says `Verdict: fail` or `Verdict: uncertain` because verdict
acceptance belongs to later review or merge policy.
`factory work review-codebase <work-item-id> <attempt-id>` appends a
review-only Attempt for full-codebase review of the current source
checkout. Review-only Attempts contain review Tasks only, read the source
checkout through workspace id `source` at path `.`, and write artifacts
under `.factory/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`.
This is the default Work-model path for review-only work; legacy review
runs remain for compatibility and recovery. The review Task executor
treats the source checkout as a guarded readable workspace: the reviewer
sandbox gets the source checkout as a read-only root and the managed
artifact area as its writable root. For no-sandbox or failed-reviewer
paths, the guard verifies that source HEAD and source files stayed
unchanged and that only the Task artifact area changed under `.factory/`.
If a reviewer changes source HEAD, the guard resets HEAD before failing
the Task. If a reviewer changes source files or protected `.factory/`
state outside the Task artifact area, the guard restores protected
checkout state before failing the Task. This restorative guard applies
only to interactive `ReviewOnly` Attempts (e.g. `factory work
review-codebase`).
Post-merge review Attempts use `AttemptKind::PostMergeReview` and a
non-restoring `PostMergeSourceGuard`. This guard verifies that the
source HEAD still matches the merged commit on completion but does not
snapshot or restore worktree changes or `.factory/` file contents.
This allows Factory and the user to write new state concurrently while
a background post-merge review is in flight. If the source HEAD moves
during the review (e.g. another merge lands), the guard fails the
review Tasks with a clear error and does not attempt restoration.
`factory work attempt run <work-item-id> <attempt-id>` is the first
Attempt-level orchestration path. It advances one Attempt by running the
next planned write Task serially through the Task executor, or by running
planned review Tasks in parallel with concurrency limited to
`FACTORY_MAX_PARALLEL_REVIEWERS` (default 5, minimum 1). Review-only
Attempts run review Tasks serially because their reviewers share a source
checkout. The loop reloads stored state before deciding the next
transition. After the
initial write output completes it plans review Tasks for the full Work
reviewer set. After a follow-up write output completes it derives the
next review roles from that Task's failed review input artifacts; when
it cannot derive at least one role, it falls back to the full Work
reviewer set. After a completed review round it interprets review
artifacts with the review subsystem verdict parser. All pass marks the
Attempt review state as passed, completes the Attempt, and creates or
returns one durable Merge Candidate for later merge execution. The Merge
Candidate records the source candidate workspace, target workspace,
source branch, target branch, candidate commit, and its own pending
review state. When the same-invocation auto-continue budget permits
another follow-up write Task, any fail records a `PlannedFollowup`
outcome, creates a planned follow-up write Task with the failed review
artifacts as Task inputs, and copies explicit Work Item instructions
into that follow-up Task, or derives those instructions from stored Work
Item planning context when explicit instructions are absent. One command
invocation may advance at most two follow-up write Tasks, including
already-planned follow-up write Tasks that existed before the command
started and follow-up Tasks planned by the command. The same invocation
then continues through each budgeted follow-up write Task, targeted
review planning, and targeted review execution until the Attempt reaches
an existing terminal boundary such as merge-candidate-ready, needs-user,
review-only complete, review-only failed, failed task, or executing-task
state. If another failed review round would require a third
same-invocation follow-up write Task, the Attempt loop marks the Attempt
`needs-user`, writes a handoff that names the failed review artifacts
and says the auto-continue budget was exhausted, and stops without
creating the over-budget Task. When no review artifact fails, uncertain
or missing verdicts mark the Attempt `needs-user` with a handoff under
`.factory/work/artifacts/<work-item-id>/<attempt-id>/`.
For review-only Attempts, all pass marks the Attempt complete with review
state `passed` and does not create a Merge Candidate. Any fail marks the
Attempt failed with review state `failed` and does not create a follow-up
write Task. Uncertain verdicts without failures mark the Attempt
`needs-user` and write the same Work handoff artifact.
`factory work list` and `factory work show <id>` expose the same durable
Work Item model for inspection. These commands use `.factory/work/items/`
through the Rust storage model and validate stored objects. This keeps
Work Items and Attempts visible while the legacy `.factory/runs`
lifecycle remains available as a fallback for full session loops and
legacy review-run recovery.
`factory status` and `factory dashboard` use Work Items as the default
operator surface. They read Work Items through `work_status.rs`, which
reduces stored Work Items to operator-facing rows. That boundary chooses
the latest Attempt, the active or waiting Task, the matching Merge
Candidate, and a short action label. It returns valid rows and per-file
read errors together so one bad Work Item file does not hide the rest of
the queue. Legacy `.factory/runs` rows remain available through
`factory status --runs` and the dashboard Runs view as an explicit
compatibility path while the old session loop remains in place.
Write Task prompt generation reads `Task.instructions` from durable Work
state and includes non-empty instructions in the coder prompt. A Task
receives those instructions from explicit Work Item instructions first,
or from rendered Work Item planning context when explicit instructions
are absent. Extra arguments passed after `--` remain coder flags only;
Factory does not append them as additional prompt text.
`factory work merge-candidate <work-item-id> <merge-candidate-id>` prints
one stored Merge Candidate as pretty JSON. This command only reads the
boundary object. `factory work merge <work-item-id> <merge-candidate-id>`
executes a Merge Candidate that still needs to land: it rebases the
candidate workspace against the target branch, runs configured pre-merge
checks in the candidate workspace, runs the required reviewer set with
merge-time context, then fast-forwards the target branch to the updated
candidate head. Merge-time review prepares one detached reviewer
worktree per role at the post-rebase candidate commit and runs those
roles in parallel. Each reviewer worktree lives at a sibling path
`../review-<work-item-id-bytelen>-<work-item-id>-<attempt-id>-<reviewer>`
relative to the project root, not nested under `.factory/work/artifacts/`.
Each reviewer sees its dedicated reviewer worktree as
the candidate workspace and receives only its own writable artifact
directory. Merge-time reviewers receive the exact
`.factory/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/reviews/<role>/review.md`
artifact path for their output and the absolute filesystem path the
reviewer must write. They also receive a shell-quoted `git -C
<workspace> diff <target-commit>..<candidate-commit>` command and a
merge-check status note; Factory does not ask them to inspect
merge-check artifact paths from the reviewer sandbox. When Factory
builds the Work merge reviewer system prompt, it uses the prompt's
`[work-system]` section when one exists and falls back to the raw
`[system]` section otherwise. Bundled reviewer prompts keep legacy
`.factory/runs` artifact paths in `[system]` and put Work-native
artifact guidance in `[work-system]`, so Work merge reviews do not
depend on filtering legacy run guidance out of bundled prompt text.
Factory then points the reviewer at the absolute candidate
workspace skill path when that skill exists; if the candidate does not
contain that skill file, the prompt tells the reviewer to apply the
reviewer role directly. If the candidate workspace contains
`.factory/expertise/decisions.md`, the prompt names that absolute path so
reviewers do not resolve decisions relative to their artifact directory.
Reviewers treat the candidate workspace as read-only and write only merge
review artifacts; scratch tests, suggested patches, and proposed
documentation edits belong in those artifacts, not in the candidate
workspace. Factory sets `CARGO_TARGET_DIR` in each reviewer's environment
to a path under that reviewer's artifact directory, so build outputs go to
`.factory/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/reviews/<role>/target/`
without reviewer cooperation. The reviewer sandbox grants read access to
the whole `.factory/work/artifacts/<work-item-id>/<attempt-id>/` subtree
so merge-check and prior-review artifacts are readable. After reviewers exit, merge execution checks each reviewer
worktree for staged, unstaged, untracked, and ignored file changes,
including changes under `.factory`, and fails before merging if any
reviewer dirtied its isolated candidate. It writes one combined review
state after all reviewer jobs finish and cleans up reviewer worktrees
after successful merge or failed review handling. After it records the
merged state, it removes the managed candidate worktree. If cleanup
fails after the target branch has merged, merge execution prints a
warning and leaves the merged Merge Candidate state intact. Running the
command again
for a Merge Candidate that already has merge status `merged` and a stored
`merged_commit` succeeds idempotently and reports the stored commit
without resolving workspaces, rerunning checks, rerunning reviewers, or
moving the target branch. Merge artifacts live under
`.factory/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/`,
and the stored Merge Candidate records whether execution is pending,
executing, failed, needs-user, or merged.

Merge execution auto-continues through failed merge-time review
rounds within a same-invocation budget. The merge loop iterates
rebase → checks → reviews. If reviewers return fail (and not a
reviewer launch panic), Factory invokes the same Coder used at
Attempt time against the candidate workspace, passing the failed
merge review artifact paths as input, asking the coder to address
the findings and commit. After the follow-up writer commits, the
loop restarts at rebase. One `factory work merge` invocation may
advance at most `MAX_MERGE_FOLLOWUP_WRITES_PER_INVOCATION = 2`
follow-up write cycles. If a third round would be needed, Factory
marks the Merge Candidate `needs-user`, writes a handoff under
`.factory/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/needs-user.md`
naming the failed review artifact paths, and bails. Reviewer launch
panics or non-verdict errors are never retried — they fail the
merge immediately.

Coder runs whose transcript contains a session-limit or rate-limit
marker are not treated as Task failures. The Coder wrapper sleeps
`FACTORY_RATE_LIMIT_RETRY_AFTER_SECS` (default 1800 seconds) and
retries the same coder invocation up to two more times before
propagating the exit code. Author and reviewer Tasks inherit this
behavior without further plumbing.
`factory cleanup` owns the terminal Work model cleanup lifecycle. It
defaults to a dry run and only mutates state with `--apply`. A Work Item
is eligible when every Attempt is terminal, every Task in those Attempts
is terminal, and every Merge Candidate is terminal. Operators can also
run `factory work abandon <work-item-id> [--reason <text>]` to mark a
stale Work Item as intentionally abandoned; cleanup treats that marker as
terminal only when no Attempt is executing or reviewing, no Task is
executing, and no Merge Candidate is reviewing or merging. Cleanup removes
the stored Work Item, referenced managed Work artifacts, managed candidate
worktrees, Work task branches, and stranded sibling reviewer worktrees
left behind by killed merges. Managed artifact references must be
relative paths made only of normal path components and must resolve under
`.factory/work/artifacts/`; cleanup ignores absolute paths and parent
escapes. It skips Work Items with active Attempts, Tasks, or Merge
Candidates, and it only removes candidate worktrees that match Factory's
managed sibling path and are registered git worktrees.

| Concept | Meaning |
|---|---|
| Work Item | Planned Factory work. Planning operates on work items. |
| Attempt | One execution history branch under a work item. Attempts are visible state and history, but they are usually not their own queue. |
| Task | Schedulable unit of work. Task kinds stay generic: `write`, `review`, `merge`, `report`, `learn`, and `probe`. Roles carry prompt and domain behavior. |
| Workspace | Factory-managed filesystem/git context. A task may read many workspaces and write at most one. |
| Merge Candidate | Candidate result prepared for merge. Its review state is separate from attempt review state. |

When artifacts or tests need to exchange a standalone task definition,
use the serialized `Task` shape from `factory::work_model` and call
`Task::validate` after parsing. This shape is an exchange contract for
the core model.

```json
{
  "id": "attempt-1-write-1",
  "kind": "write",
  "role": "author",
  "instructions": "Preserve coder flags as args.\nKeep prompt content in Work state.",
  "work_item_id": "work-1",
  "attempt_id": "attempt-1",
  "workspace_access": {
    "reads": [],
    "writes": [
      {
        "id": "candidate",
        "path": "../work-6-work-1-attempt-1"
      }
    ]
  },
  "status": "complete",
  "output": {
    "workspace_id": "candidate",
    "workspace_path": "../work-6-work-1-attempt-1",
    "source_branch": "main",
    "commit": "0123456789abcdef"
  }
}
```

The `kind` field accepts only `write`, `review`, `merge`, `report`,
`learn`, or `probe`. `workspace_access.reads` may list any number of
workspaces. `workspace_access.writes` may be empty or contain one
workspace. A `review` task must keep `writes` empty; reviewers write
findings and notes under a required `artifact_area`.

Write Tasks may include optional `instructions` copied from explicit Work
Item instructions or derived from Work Item planning context. JSON omits
`instructions` when the Task has no rich execution context.

`status` tracks Task lifecycle state: `planned`, `executing`,
`complete`, `failed`, or `needs-user`. Planned Tasks omit the field in
JSON. Completed write Tasks include `output`, which records the writable
workspace id and path, the source branch resolved from the project root
when the Task run started, and the commit that contains the Task output.
Planned review Tasks include `review_context`, copied from that write
output, with the candidate workspace id and path, source branch, and
candidate commit. Follow-up write Tasks include `input_artifacts` when
reviewers fail an Attempt; each entry names the producing review Task
and the artifact path, such as
`.factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md`.
The Attempt loop uses those producer task ids to choose the reviewers
for the next follow-up review round. When Factory plans that targeted
round, each review Task receives the matching prior failed review
artifact for its role in `input_artifacts`, so the executor can prompt
the reviewer to verify the follow-up against the concrete findings and
grant sandboxed read access to that artifact.
JSON omits `input_artifacts` when the list is empty. Incomplete Tasks do
not carry output. Attempt
completion is derived from its Tasks; a complete Attempt must not contain
unfinished Tasks, and completing one Task does not by itself complete an
Attempt that still has unfinished Tasks. Completed review Tasks record
artifact references under the Attempt and do not set `output`; their
markdown verdict remains data, not Task execution status.

Review tasks are read-only with respect to candidate workspaces,
including managed candidate workspaces beside the source checkout.
Factory grants sandboxed review Tasks read-only access to candidate
worktrees and their shared git metadata so standard commands such as
`git diff <source>..<candidate>` work from the candidate workspace. They
may write task artifacts, such as findings or scratch notes, but concrete
reviewer fixes become follow-up `write` tasks. Sandboxed delegated tasks
produce learning artifacts in task artifact areas; Factory can later
ingest those artifacts into project-local expertise after review.
Work-model behavior reviewers receive the Work Item behavior increment
from `WorkItem.planning_context.behaviors` in the prompt when one exists,
or an explicit statement that no behavior increment was provided. Legacy
`.factory/runs/<run-id>/behaviors.diff.md` remains a legacy run input,
not the Work-model behavior review contract.

Project-local `.factory/observations.md` and `.factory/expertise/*` are
durable Factory memory that belongs in normal repository history. Legacy
runtime compatibility state remains under `.factory/runs` and related
run artifacts. Keeping these concepts separate lets learning and
planning land through the same reviewed workflow as code without
treating transient session state as project knowledge.

Durable work model state lives under `.factory/work/`. This tree is
separate from `.factory/runs`, which still stores legacy run execution
state, session artifacts, reviewer state, worktree handles, and status
files. The Work bridge does not migrate run directories. Existing commands
keep supporting `.factory/runs` without requiring `.factory/work/`; the
coexistence is a compatibility bridge while agents start using Work Items,
Attempts, Tasks, Workspaces, and Merge Candidates for new delegated build
work.

Managed candidate worktrees do not live under `.factory/work/`. Factory
keeps Work Item JSON, review artifacts, merge artifacts, and operator
state in the source checkout, but it creates candidate git worktrees as
sibling directories beside the source checkout. Stored workspace
references remain relative to the source root, for example
`../work-6-work-1-attempt-1`, and include a Work Item ID byte-length
prefix, Work Item ID, and Attempt ID so valid hyphenated IDs remain
globally distinct.

The Work storage contract is:

```
.factory/work/
  items/
    <work-item-id>.json
  attempts/
    <work-item-id>/
      <attempt-id>.json
  tasks/
    <work-item-id>/
      <attempt-id>/
        <task-id>.json
  merge-candidates/
    <work-item-id>/
      <merge-candidate-id>.json
  artifacts/
    <work-item-id>/
      <attempt-id>/
        <task-id>/
        <merge-candidate-id>/merge/
```

Each file in `items/` stores Work Item metadata and planning context:
the Work Item id, title, optional explicit instructions, optional
brief/behaviors/approach/plan context, and optional abandonment marker
with the operator-provided reason. Attempts live in
`attempts/<work-item-id>/<attempt-id>.json`, Tasks live in
`tasks/<work-item-id>/<attempt-id>/<task-id>.json`, and Merge Candidates
live in `merge-candidates/<work-item-id>/<merge-candidate-id>.json`.
`WorkModelStore` assembles those split records into the public
`WorkItem` shape from `factory::work_model` for `factory work show`,
status, dashboard, task execution, review, merge, and cleanup.
If an item file contains nested Attempts, Tasks, or Merge Candidates,
Factory ignores those nested operational collections and exposes only
records from the split Attempt, Task, and Merge Candidate collections.
Attempt records carry `kind`, which serializes as `write` or
`review-only`; older records and omitted values default to `write`.
Write attempts omit `kind` on new writes, and review-only attempts
persist `kind: "review-only"` so the attempt loop can interpret review
verdicts as terminal output instead of post-author feedback. Attempt
records also carry an internal `order` field so the assembled public
Work Item preserves append order after Factory reloads split records.
Task records carry the same internal `order` field; Factory sorts split
Task files by that persisted order before exposing `Attempt.tasks`.
Factory writes Work Item metadata to `items/` and operational records to
the split Attempt, Task, and Merge Candidate collections.
When `WorkModelStore` reads stored Work state, it normalizes legacy
artifact references that still use
`.factory/work/artifacts/<attempt-id>/...` into the current
`.factory/work/artifacts/<work-item-id>/<attempt-id>/...` form before it
validates the assembled Work Item. If the legacy path exists on disk and
the namespaced path does not, the store moves that artifact directory or
file into the Work Item namespace during the read.

Tasks store their workspace access under `workspace_access.reads` and
`workspace_access.writes`. Workspace references stay inside task
`workspace_access.reads` and `workspace_access.writes` and point to
managed sibling worktrees for candidate execution. Merge Candidates store
the reviewed source candidate workspace and target workspace directly as
boundary data derived from the passed Attempt's latest completed write
Task. Factory does not keep a standalone workspace registry in this
contract. Merge Candidates use the public `MergeCandidate` shape and have
their own candidate collection.

Code that reads `.factory/work/` state must parse records into the public
Rust model and validate every assembled Attempt, Task, and Merge
Candidate before using the object.
The merge executor is the only recovery reader in this contract: it may
load a Work Item that fails merge-execution preconditions so it can mark
the affected Merge Candidate failed, but it must validate the candidate
before it updates a workspace or target branch. Failed Merge Candidates
still must preserve the boundary data derived from the latest completed
write Task; the failed state only records why merge execution stopped.
The `WorkItem.id` inside each item file must match the file stem, so
`.factory/work/items/work-1.json` must contain `"id": "work-1"`.
Attempt, Task, and Merge Candidate ids must match their file stems.
Work item IDs, Attempt IDs, Task IDs, and Merge Candidate IDs must not be
empty, `.`, `..`, or contain `/` or `\`, because Factory uses each ID as
one path component under the split collections. Each stored Attempt must
set `work_item_id` to the containing `WorkItem.id` and store its append
position in `order`. Each stored Task must set `work_item_id` to the
containing `WorkItem.id`, and must set `attempt_id` to the containing
Attempt id even though the public Task shape allows `attempt_id` to be
absent before a task joins an Attempt.
Invalid JSON, ID mismatches, invalid object IDs, and model validation
failures must report the file path or object that failed. Code that
writes Work state must use deterministic pretty JSON and must not write
invalid model state.

## Legacy run compatibility

The legacy recursive run model remains for compatibility, Fargate-only
execution, coordinated child-run decomposition, and recovery of existing
`.factory/runs` state. New delegated build work should use Work Items,
Attempts, Tasks, Workspaces, and Merge Candidates instead.

```
Brief
  └── Run (top-level)
        ├── Requirements
        ├── Plan
        └── Run  Run  Run    ← plan spawns child runs
```

Each run executes in its own git worktree, branched from whatever the user
is working on. The worktree is a sibling of the source worktree:

```
project/
  main/                      ← source worktree
    .factory/
      active-run             ← current run-id
      runs/
        run-20260507/
          brief.md
          behaviors.diff.md
          approach.md
          plan.md
          status
          source-branch      ← "main"
          worktree           ← "../run-20260507"
  run-20260507/              ← run worktree (created at launch)
    .factory/
      active-run
      runs/run-20260507/     ← copied from source
    src/                     ← agent works here
```

When done, merging a worktree run executes the project's
`check-pre-merge` hook (if present) against the worktree, copies
artifacts back from the worktree, removes the worktree, rebases the
run branch onto the source branch, fast-forward merges, deletes the
branch, and sets the status to `merged`. This policy applies to
normal `factory merge` runs and to child runs that the parallel
orchestrator lands after each group completes.

Projects wire pre-merge verification through the hooks system. A
single executable at `.factory/hooks/check-pre-merge` gates merging:
exit 0 lets it proceed, non-zero stops before any destructive step.
A sibling executable at `.factory/hooks/fix-pre-merge` (optional)
is invoked when `check-pre-merge` fails. Factory requires the
worktree to be clean before running the fix hook, commits any
changes the fix produces outside `.factory/`, reruns
`check-pre-merge`, reruns reviewers after the fix commit, and
continues only when the required checks and reviews pass. The
project decides what each script invokes — `cargo fmt`,
`make ci`, `pre-commit run --all-files`, anything else.

Example:

```sh
#!/bin/sh
# .factory/hooks/check-pre-merge
set -e
cargo fmt --all -- --check
```

```sh
#!/bin/sh
# .factory/hooks/fix-pre-merge
cargo fmt --all
```

### Run state

| File | Purpose |
|---|---|
| `brief.md` | User's intent |
| `behaviors.diff.md` | New behaviors this run adds |
| `approach.md` | Solution direction and expertise references |
| `plan.md` | Execution steps |
| `status` | `briefed`, `behaviors-defined`, `approach-designed`, `planned`, `executing`, `reviewing`, `rate-limited`, `needs-user`, `complete`, `failed`, `merged` |
| `handoff.md` | Context for the next session |
| `active-run` | Current run-id (in `.factory/`) |
| `source-branch` | Branch the run forked from |
| `worktree` | Path to the run's worktree |
| `runtime` | `local` or `fargate` |
| `coder` | `claude` or `codex` |
| `handle` | Runtime-specific identifier |
| `mode` | `review` or absent (defaults to full lifecycle) |
| `reviewers` | Comma-separated reviewer filter (optional) |
| `scope` | Review focus targeting (optional) |
| `sessions.log` | Per-session metadata: `{timestamp} session=N exit=CODE duration=Xs status=STATUS` and review-phase entries: `{timestamp} review=N duration=Xs verdict=VERDICT` |
| `report.md` | Generated run report |
| `cleaned.md` | Cleanup context written after `factory cleanup --apply` preserves the run directory and status |
| `reviews/` | Current review artifacts, transcripts (`transcript-{name}.jsonl`), and prior round archives (`round-N/`) |
| `review-state.json` | Effective outcome of the latest review phase |
| `children` | Child run IDs, one per line (written by the parallel orchestrator for parent runs) |
| `parent` | Parent run ID (written for each child run) |

### Source and live artifacts

The source run directory under `.factory/runs/<id>` remains the registry
for known runs and durable metadata such as `worktree`, `runtime`,
`coder`, `source-branch`, merging records, and cleanup records. When the
source run directory has a `worktree` file that points at an existing
worktree containing `.factory/runs/<id>/`, Factory treats that worktree
run directory as the live artifact directory for current session-loop
state.

Commands that read current run progress ask the run model for effective
artifacts. Effective reads prefer the live worktree run directory for
`status`, `sessions.log`, `sessions/`, `reviews/`, `review-state.json`,
`handoff.md`, and `report.md`, then fall back to the source run
directory when the live artifact does not exist or the worktree pointer
is invalid. This shared rule covers status listings, watch
notifications, summaries, implicit resume selection, headless resume,
mergeable-run scans, and review checks before merging. Dashboard views
use the effective status and review-state rules and fall back from live
`report.md` to source `report.md`; transcript and current reviewer tabs
list artifacts from the resolved live artifact directory.

### Run-id resolution

The factory command resolves the run-id through a priority chain:

1. `--run-id` flag
2. `FACTORY_RUN_ID` environment variable
3. `.factory/active-run` pointer file
4. Scan `.factory/runs/` for active status (fallback)

### Run summary

`factory summary` resolves the active run through the standard run-id
resolution chain and prints a compact text snapshot from durable run
artifacts. `factory summary --run-id <id>` summarizes that run directly.
The summary uses the shared source-and-live artifact rule for current
status, sessions, reviews, handoff, and report presence.

The summary intentionally avoids transcript or report dumps. It includes
the run phase, brief excerpt, author metadata from `coder`, reviewer
activity, child run activity from `children`, latest `sessions.log`
entries, the effective review state from `review-state.json`, the first
actionable handoff line or open question, whether `report.md` exists,
and a rule-based next action. When `review-state.json` is absent, the
summary falls back to top-level `reviews/review-*.md` verdicts so old
runs remain readable. This makes the command useful in a terminal and
keeps the same data shape available for later dashboard or
reporting-agent integration.

### Session continuity

The factory command checks for a parallel plan before entering the session
loop. If `plan.md` exists and describes multiple groups or any parallel
group with more than one step, execution takes the orchestrator path instead.

**Serial path** (default — single run, session loop):

```
while run is not complete:
    launch author with the selected coder in non-interactive JSON mode
    pipe stdout to sessions/session-N/transcript.jsonl
    author works until context exhaustion or completion
    author writes handoff.md + status file
    write session metadata to sessions.log
    if status is complete:
        if no committed, staged, unstaged tracked, or untracked changes exist
           and no explicit review scope exists: set status to complete, stop
        set status to reviewing
        run review phase (all reviewers in parallel)
        if all pass and worktree is clean outside .factory:
            set status to complete, stop
        else if all pass and worktree is dirty outside .factory:
            write handoff.md, set status to executing, restart
        else:
            set status to executing, restart with findings
    if terminal status (needs-user, failed): stop
    if executing: restart
    if rate-limited: wait 5 minutes, restart
```

**Parallel path** (orchestrator — parent run with child runs):

```
for each group in plan:
    create child run for each step (run dir, worktree, brief)
    if group is parallel: launch all children concurrently
    else: run children one at a time
    wait for all children to complete
    if any child failed: set parent to failed, stop
    run pre-merge checks and merge each child's branch into parent branch
    set each child's status to merged
record children list in parent run dir
set parent status to complete
```

The parent run's session loop never executes — the orchestrator
(`parallel::run_parallel_plan`) replaces it entirely. Each child run
gets its own session loop in its own worktree.

After the orchestrator completes, all children are already merged and
merged. `factory merge` on the parent run verifies all children are
merged and sets the parent status to `merged` — there is no worktree
to remove or branch to rebase for the parent itself.

The agent writes one word to `status` before exiting. The loop reads that
word. That's the entire contract.

### Session directories

Each session produces a single artifact:

```
.factory/runs/[run-id]/sessions/
  session-1/
    transcript.jsonl     ← JSON event output (piped from agent stdout)
  session-2/
    ...
```

The transcript is the stream-json verbose output captured during the
session. Global `~/.claude` state (history, memory, todos, plans) is not
copied into session directories.

### Review scope

Reviewers examine either the run's changes or the full codebase:

- `ReviewScope::Changes` — review only the diff produced by this run.
  Used in the normal post-execution review phase.
- `ReviewScope::Full` — review the entire codebase. Used by review-mode
  runs.

When a run-scoped review triggers but no code has changed and no
explicit scope file was provided, the review phase is skipped entirely.
Factory treats the run as changed when the run branch has committed
differences from the source branch, or when `git status --porcelain`
reports staged changes, unstaged tracked changes, or untracked
non-ignored files outside `.factory` in the run worktree. This avoids
wasting reviewer sessions on runs that only modified run state files
while still reviewing dirty author output.

An author-session run can only finish as `complete` with a clean
worktree. If reviewers pass while staged, unstaged, or untracked
non-ignored files remain, the session loop writes a handoff and moves
the run back to `executing` so the next author session can commit,
revert, or intentionally ignore the remaining work. Review-only runs do
not launch an author to modify the worktree; passing review-only runs
set status to `complete`, and non-passing review-only runs set status to
`failed`. The merging path also rejects dirty completed worktrees before
removing them, so uncommitted author output is not discarded during
land.

## Version Metadata

`factory version` prints a single line:

```sh
factory 0.1.0 abc1234
```

The first field is the literal `factory` command name. The second field
is the package version from `Cargo.toml`. The third field is the short
Git commit captured by `build.rs` when Cargo builds the binary. If Git
is unavailable at build time, Factory prints
`unknown` in the commit field.

## Agents

### Coder selection

Local runs support Claude Code and OpenAI Codex. Claude remains the
default for compatibility. Select Codex with `--coder codex` or
`FACTORY_CODER=codex`. The factory records the selected coder in the
run's `coder` file.

Claude sessions use `claude -p --append-system-prompt` with stream-json
output. Sandboxed Claude sessions run inside the macOS Seatbelt profile
that Factory renders for the run worktree plus the source repository's
common git directory. The worktree root lets the agent edit project
files; the common git directory lets linked worktrees update branch,
index, and worktree metadata without granting write access to unrelated
sibling worktrees.

Codex sessions use `codex --ask-for-approval never exec --json --cd <worktree>`
and receive the factory system prompt prepended to the session prompt
because the Codex CLI has no Claude-style append-system-prompt flag.
`--ask-for-approval` is a top-level Codex option and must appear before
`exec`. Sandboxed local Codex runs are wrapped by Factory's macOS
Seatbelt profile with the same writable roots as Claude: the run
worktree and source repository common git directory. Factory passes
`--dangerously-bypass-approvals-and-sandbox` to Codex in this mode so
Codex does not apply its own sandbox or pause for approvals inside the
Factory sandbox. Factory also sets `SSL_CERT_FILE` for sandboxed Codex
using a file-based CA bundle so Codex can connect without Keychain IPC.
`FACTORY_CODEX_CA_BUNDLE` overrides the detected bundle path and any
caller-provided `SSL_CERT_FILE`. In bare mode, Codex also runs with
`--dangerously-bypass-approvals-and-sandbox`, but without
`sandbox-exec`.

Fargate currently supports only Claude because its container entrypoint
and credential path remain Claude-specific. The Fargate run image builds
the Rust Factory binary during the Docker image build and copies it to
`/usr/local/bin/factory`; the task entrypoint uses that binary to enter
the shared Rust session loop. The Fargate wrapper owns durable runtime
metadata, so the in-place local loop does not rewrite `runtime` or
`handle` while it runs inside the task.

Sandboxed local Claude runs refresh Claude OAuth credentials outside the
sandbox at session boundaries. Sandboxed local Codex runs do not use that
Claude refresh hook.

### Author

Implements code. Follows the plan. Pauses when genuinely uncertain rather
than drifting.

### Reviewers

Evaluate the author's output. Five reviewers run in parallel, each
following its own skill:

- Documentation (code-aware) — reads code and docs, checks accuracy,
  writing quality, and completeness.
- Behaviors (user-facing) — observes behavior only, cannot see code.
  Evaluates the system from the outside, as a user would.
- Architecture (code-aware) — reads code and architectural expertise,
  evaluates structural decisions against principles.
- Skills (code-aware) — reads skill files and checks them against
  `references/skills.md` for structure, quality, and spec compliance.
- Tests (code-aware) — reads tests and evaluates coverage, isolation,
  structure, and adherence to testing principles.

Review verdicts: **pass** / **uncertain** (ask user) / **fail** (send
back to author with findings).

When the author receives findings from multiple reviewers, it weighs
each finding according to the reviewer's domain expertise. When reviewers
disagree, the one with relevant expertise for that finding takes priority.
The author escalates to `needs-user` only when genuinely stuck.

### Review phase

The session loop evaluates review eligibility when the author sets
status to `complete`. It skips run-scoped reviews only when the user did
not request an explicit review scope and the run worktree has no
committed, staged, unstaged, or untracked non-ignored changes. Otherwise
reviewers run in parallel, each producing an artifact in
`.factory/runs/[run-id]/reviews/`. The review lifecycle records the
effective outcome in `.factory/runs/[run-id]/review-state.json` with the
state, round, source, and per-reviewer verdicts. Consumers use that file
as the review boundary when it exists; old runs without it fall back to
top-level `reviews/review-*.md` verdicts.

The review subsystem owns verdict parsing and acceptance rules:
`review.rs` reads `review-state.json`, falls back to current
`reviews/review-*.md` artifacts for old runs, and decides whether the
effective review outcome is accepted. `run.rs` does not interpret review
verdicts directly; it resolves source versus live worktree artifact
locations and delegates review acceptance to `review.rs`. This keeps
durable run status (`status`) separate from review outcome semantics.

The loop parses each reviewer's verdict:

- All pass: the run completes only if the worktree is clean; if
  uncommitted changes remain outside `.factory`, the loop writes a
  handoff, sets status back to `executing`, and restarts the author to
  resolve them.
- Any fail or uncertain: status resets to `executing`, the author
  restarts with instructions to read and address the review findings.
- Reviewer execution failure: missing prompts, launch errors, non-zero
  exits, missing review artifacts, reviewer errors, and reviewer thread
  panics count as non-passing review results. The review lifecycle
  writes a current-round `reviews/review-[name].md` artifact with
  `Verdict: fail` for each operational reviewer failure, then records
  the effective non-passing outcome in `review-state.json`.

If the run exceeds the review-round limit, the loop accepts the current
review state with the same clean-worktree guard: clean work completes,
while uncommitted work receives a handoff and returns to `executing`.
Clean review-limit completion records `state:
accepted-review-limit`, `source: review-limit`, `max_rounds`, and a
short reason in `review-state.json`. Dirty review-limit completion does
not write that acceptance state.

When a new review round starts, the review lifecycle moves the previous
round's top-level `review-*.md` and `transcript-*.jsonl` files into
`reviews/round-N/`. The top-level `reviews/` directory therefore
represents only the current review round; archived `round-N/` contents
remain historical records and do not drive current dashboard reviewer
tabs or verdicts.

Use `factory work review-codebase <work-item-id> <attempt-id>` for new
full-codebase review-only work. `factory review` remains a compatibility
and recovery path for legacy `.factory/runs` review runs. It creates or
reuses a review run, writes `status` as `planned`, `mode` as `review`,
updates `.factory/active-run`, and writes optional `reviewers` and
`brief.md` files from `--reviewers` and `--brief`. After preparing that
state, it enters the normal local run loop for the selected coder and
sandbox mode.

Legacy review runs (`mode=review`) produce findings only. Reviewers run
with full-codebase scope. Their findings are written to the reviews/
directory. Passing review-only runs set status to `complete`;
non-passing review-only runs set status to `failed`. No author session is
launched.

### Resume

`factory resume` without a run ID finds a run with status `needs-user`
or `failed`. `factory resume [RUN_ID]` selects the named run directly.
After selecting a run, Factory chooses the resume path from stdin. With a
terminal on stdin, it launches an interactive agent session with the
selected coder so the user can provide input or unblock the run.

Without a terminal on stdin, `factory resume` restarts the selected run
through the local session loop instead of launching an interactive
agent. When the run records a worktree, the loop uses that worktree and
its copied run directory. Otherwise it falls back to the command's
search root and the source run directory. The loop captures the
transcript, continues session numbering from existing run state, and
keeps the normal status, handoff, and review handling. Headless resume
rejects parallel parent runs because their session loop never executes;
their child runs own the resumable work.

## Runtimes

### Local

The factory command runs the session loop on the local machine. Claude
and Codex run inside a macOS Seatbelt sandbox rendered by Factory.
Factory renders each sandbox from `common.sb` plus the selected coder's
profile layer: `claude-code.sb` for Claude Code and `codex.sb` for
Codex.
Claude uses the Claude token refresh hook at session boundaries; Codex
does not.

### Local (bare)

`factory run --no-sandbox` runs the session loop without Seatbelt
sandboxing, Codex sandboxing, or credential refresh. A git worktree is
still created when the directory is a git repo. Used on platforms
without local sandbox support or when the agent is already isolated by
other means. Claude runs with `--dangerously-skip-permissions`; Codex
runs with `--dangerously-bypass-approvals-and-sandbox`.

### Fargate

Single-container model on AWS ECS Fargate.

The `fargate_bootstrap.rs` module deploys infrastructure and builds
images just-in-time on the first `--runtime fargate` invocation
(and again whenever inputs change). The Dockerfile at
`infrastructure/run/Dockerfile` compiles the Rust Factory binary in
a builder stage and copies it into the task image at
`/usr/local/bin/factory`, so task startup only transfers the
workspace and invokes the binary.

```
Local machine                    Fargate task
─────────────                    ────────────
1. create worktree
2. upload worktree → S3
3. start task ────────────►
                                 4. pull workspace from S3
                                 5. write runtime=fargate and task handle
                                 6. /usr/local/bin/factory run
                                    --runtime local
                                    --no-sandbox --in-place
                                    --preserve-run-metadata
                                    --coder claude
                                 7. Rust session loop launches Claude
                                 8. ...hours pass...
                                 9. upload workspace → S3
factory status --runs ───► (local run artifacts)
factory shell ───────────► (ECS Exec into container)
factory pull ────────────► (download from S3 into worktree)
```

#### IAM permissions (minimal)

| Permission | Scope | Purpose |
|---|---|---|
| `s3:GetObject` | `runs/*`, `work/*`, `work-merge/*` | Pull input workspace |
| `s3:PutObject` | `runs/*`, `work/*`, `work-merge/*` | Upload completed workspace |
| `s3:*` Deny | Outside the allowed prefixes | Explicit deny on everything else |
| `ssmmessages:*` | `*` | Accept incoming ECS Exec sessions |

Six actions total. No ECS, IAM, STS, or other AWS permissions. The
container can be connected to (ECS Exec) but cannot connect out to other
containers via SSM. `work/` covers Work Attempt artifacts and
`work-merge/` covers Merge Candidate artifacts; the legacy `runs/`
prefix remains for the existing legacy `factory run --runtime fargate`
flow.

#### Infrastructure (CloudFormation)

- 1 ECR repository (`factory/run`)
- 1 ECS cluster
- 1 task definition (1 vCPU, 2 GB RAM, 30 GB ephemeral storage)
- 1 S3 bucket (30-day lifecycle)
- 1 IAM task role (6 actions)
- 1 IAM execution role (ECR pull + logs)
- 1 security group (egress only)
- CloudWatch log group (optional, infra debugging)

No EFS. Fargate ephemeral storage is sufficient for a single container.

#### Work model on Fargate

The Work model Fargate path uses a worktrees-root layout that
matches the local layout (project root + sibling candidate/review
worktrees, all under a single parent directory).

Container layout:

```
/worktrees/
├── ${FACTORY_PROJECT_NAME}/              project root
├── work-<bytelen>-<id>-<attempt>/        candidate worktree
└── review-<bytelen>-<id>-<attempt>-...   review worktrees
```

`FACTORY_PROJECT_NAME` is the basename of the local project root
(e.g. `main`) passed as a task environment override. The container's
`WORKSPACE` then resolves to `/worktrees/${FACTORY_PROJECT_NAME}`, so
Factory's `initial_candidate_workspace_path = "../<name>"` naturally
lands siblings at `/worktrees/work-...` and `/worktrees/review-...`
beside the project root.

Local layout mirrors this:

```
<project_root>/..  (e.g. /Users/mrinal/Workspace/factory/)
├── main/                                project root
├── work-<bytelen>-<id>-<attempt>/
└── review-<bytelen>-<id>-<attempt>-...
```

Tarball format (symmetric input and output): one top-level entry per
worktree, no wrapper directories.

| Direction | Tar `-C` directory | Contents |
|-----------|--------------------|----------|
| Local → S3 (input) | `<project_root>/..` | the project basename only |
| S3 → container (input) | `/worktrees` | files restored under `/worktrees/<project>/` |
| Container → S3 (output) | `/worktrees` | project + any sibling worktrees |
| S3 → local (output) | `<project_root>/..` | project overwritten and siblings restored |

Commands:

```
factory work attempt run   --runtime fargate <work-item-id> <attempt-id>
factory work attempt watch                   <work-item-id> <attempt-id>
factory work attempt pull                    <work-item-id> <attempt-id>
factory work attempt stop                    <work-item-id> <attempt-id>

factory work merge   --runtime fargate <work-item-id> <candidate-id>
factory work merge-watch                <work-item-id> <candidate-id>
factory work merge-pull                 <work-item-id> <candidate-id>
factory work merge-stop                 <work-item-id> <candidate-id>
```

The local launcher uploads the project workspace to
`s3://<bucket>/work/<work-item-id>/<attempt-id>/workspace-in.tar` (or
`work-merge/<work-item-id>/<candidate-id>/workspace-in.tar`), launches
the ECS task with `FACTORY_WORK_ITEM_ID`, `FACTORY_PROJECT_NAME`, and
either `FACTORY_WORK_ATTEMPT_ID` or `FACTORY_WORK_MERGE_CANDIDATE_ID`,
and records the task ARN under
`.factory/work/runtime/{attempts,merges}/<id>/.../fargate-task-arn`.

`watch` polls `aws ecs describe-tasks` until `lastStatus=STOPPED`,
printing transitions and the final `stopCode`/`stoppedReason`.

`stop` reads the recorded task ARN and calls `aws ecs stop-task`. The
call is idempotent: an already-stopped or absent task returns Ok.

After changes to `entrypoint.sh`, the base image's Dockerfile, or
the Factory binary, the next `--runtime fargate` invocation detects
the input change via the hash recorded in
`~/.config/factory/fargate.state.json` and rebuilds + pushes the
base image automatically. A rebuilt base also triggers a rebuild
of any project image that FROMs it. The `FACTORY_FARGATE_FORCE_REBUILD`
environment variable forces the chain regardless of cached state.

#### Just-in-time bootstrap

`src/fargate_bootstrap.rs::ensure_setup` is called before every
Fargate launch. It is idempotent: on first use it discovers the
default VPC and subnets, deploys the CloudFormation stack named
`factory`, reads stack outputs (cluster ARN, task-definition ARN,
ECR repository URI, S3 bucket, security group), authenticates
Docker with ECR, builds the Factory base image from the embedded
`infrastructure/run/Dockerfile` (tagged both as
`<repo>:latest` and as `factory-runtime:latest` for the local
build context), pushes it, and writes everything to
`~/.config/factory/fargate.state.json`. The state file records
the deployed region, stack output values, a hash of the base image
inputs, and per-project hashes of `.factory/Dockerfile`. On later
invocations Factory recomputes the hashes and only rebuilds when
they change. The Factory source tree must be locatable: either
`FACTORY_SOURCE_ROOT` is set explicitly, or Factory walks up from
the project root looking for a directory that contains both
`Cargo.toml` and `infrastructure/run/Dockerfile`.

#### BYO project Dockerfile

Each Factory-managed project that needs project-specific toolchains
in its Fargate container provides `.factory/Dockerfile`. Factory's
bootstrap builds that Dockerfile after the base image is in place,
tags it as `<repo>:project-<project-name>`, and pushes. The
project Dockerfile uses a literal `FROM factory-runtime:latest` —
Factory tags the just-pushed base image locally before invoking
`docker build` so the FROM resolves. The base image and project
image lifecycles are coupled: when the base image hash changes,
all project images that FROM it are rebuilt on their next launch.
Projects with no `.factory/Dockerfile` use the base image
directly and have access only to what the base ships
(`factory` binary, `aws-cli`, `git`, `bash`, `jq`, `tmux`,
`curl`, `claude-code`).

## Credential management

### Local runtime

| Credential | Source | Method |
|---|---|---|
| Claude OAuth | macOS Keychain | Extract, pass as env var. Refresh via unsandboxed `claude -p "ok" --max-turns 1` at session boundaries. |
| AWS | SSO profile | `aws configure export-credentials` resolves to STS temps, passed as env vars. |
| Brave Search | macOS Keychain | Extract, pass as env var. |

Sandbox profile unchanged — credentials injected via env vars, never by
opening filesystem access.

### Fargate runtime

Claude OAuth token passed as env var at task launch. Short-lived; multi-hour
runs will outlive it. Future: WIF (Workload Identity Federation) for
automatic token refresh using the task's IAM identity.

## Repository structure

```
factory/main/
  CLAUDE.md
  build.rs                   ← emits build-time metadata
  Cargo.toml                 ← Rust crate definition
  Cargo.lock
  src/
    main.rs                  ← CLI dispatch (clap)
    lib.rs                   ← public API for tests
    coder.rs                 ← Coder trait + Claude/Codex implementations
    cli.rs                   ← CLI argument types
    cleanup.rs               ← Cleanup of terminal run and Work state
    content.rs               ← Runtime content resolution (project → user → bundled)
    credential.rs            ← Keychain credential injection
    fargate_bootstrap.rs     ← JIT Fargate setup (CFN, base + project image builds)
    hooks.rs                 ← Project hook execution (.factory/hooks/<name>)
    merge.rs                  ← Merging policy and pre-merge hook orchestration
    run.rs                   ← Run state, resolution, status
    session.rs               ← Session loop orchestration
    review.rs                ← Review loop, verdict parsing
    os.rs                    ← Seatbelt sandbox rendering, prerequisites
    worktree.rs              ← Git worktree operations
    report.rs                ← Report generation
    fargate.rs               ← Fargate launch, pull, shell
    dashboard.rs             ← Live TUI for run activity
    summary.rs               ← Text run summary from durable artifacts
    transcript.rs            ← Parse stream-json transcripts incrementally
    work_model.rs            ← Core Work Item / Attempt / Task model
    work_status.rs           ← Summarize Work Items for status and dashboard
    work_merge_executor.rs   ← Execute Work Merge Candidates
    work_task_executor.rs    ← Execute Work Tasks
    work_attempt_loop.rs     ← Advance one Work model Attempt
    plan.rs                  ← Parse plan.md into groups and steps
    parallel.rs              ← Parallel plan orchestrator (child runs)
    version.rs               ← Version command output format
  documentation/
    architecture.md          ← this file
    behaviors.md             ← behavioral statements (EARS)
  expertise/                 ← factory-level (applies to all projects)
    architecture.md
    documentation.md
    shell-scripts.md
    skills.md
    terminal-ui.md
    tests.md
  .factory/
    observations.md          ← feedback log (tracked)
    expertise/               ← project-level learnings (tracked)
    hooks/                   ← project hook scripts (tracked)
      check-pre-merge
      fix-pre-merge
    runs/                    ← working state (not tracked)
    work/                    ← Work model durable state (not tracked)
  prompts/                   ← agent system prompts
    author.md
    review-architecture.md
    review-behaviors.md
    review-documentation.md
    review-skills.md
    review-tests.md
  sandboxes/                 ← Seatbelt profile templates
      common.sb              ← Shared Seatbelt profile template
      claude-code.sb         ← Claude-specific Seatbelt profile layer
      codex.sb               ← Codex-specific Seatbelt profile layer
  skills/
    architect/SKILL.md
    architect/references/
    build-in-the-factory/SKILL.md
    capture-brief/SKILL.md
    define-behaviors/SKILL.md
    design-approach/SKILL.md
    design-approach/references/
    plan-execution/SKILL.md
    review-architecture/SKILL.md
    review-architecture/references/   ← symlinks to expertise/ (dereferenced on install)
    review-behaviors/SKILL.md
    review-documentation/SKILL.md
    review-documentation/references/
    review-skills/SKILL.md
    review-skills/references/
    review-tests/SKILL.md
    review-tests/references/
    test-terminal-ui/SKILL.md
    test-terminal-ui/references/
    write-documentation/SKILL.md
    write-documentation/references/
    write-tests/SKILL.md
    write-tests/references/
  infrastructure/
    cloudformation.yaml
    run/
      Dockerfile
      entrypoint.sh
    teardown.sh
  tests/
    behaviors/
      operations/            ← behavioral tests for the Rust binary
      skills/                ← scenario cards for test-skill
      README.md              ← behavior-to-test mapping
```

## Active module responsibilities

Several modules own operational policy that would otherwise blur across
the CLI, run model, and git helpers.

### Project hooks

`hooks.rs` is the project-hook execution surface. Factory invokes
executable scripts at `.factory/hooks/<name>` at known lifecycle
events. The naming convention encodes both the action and the
phase: `check-pre-<phase>` are gates (non-zero exit blocks the
phase), `fix-pre-<phase>` are autofixes (run when the matching
`check-pre-<phase>` failed), and `post-<phase>` are notifications
(non-zero exit is logged but does not block). The `<phase>` suffix
aligns with existing Factory state vocabulary (`land`,
`attempt-failed`, `merge-needs-user`, `write`, `review`).

Each hook receives Factory context as environment variables
(`FACTORY_HOOK`, `FACTORY_WORK_ITEM_ID`, `FACTORY_ATTEMPT_ID`,
`FACTORY_TASK_ID`, `FACTORY_MERGE_CANDIDATE_ID`,
`FACTORY_CANDIDATE_COMMIT`, `FACTORY_ARTIFACT_DIR`) and runs with
the candidate workspace as its working directory. Stdout and
stderr are captured to `<log_dir>/<hook-name>.log` so failures stay
inspectable after the fact. Hooks that are missing or not
executable are silently skipped — no central registry, no
configuration file, the filesystem is the manifest.

### Merging

`merge.rs` owns the policy that happens immediately before a run
branch is merged. It calls the `check-pre-merge` hook (if present)
against the recorded run worktree and proceeds with merging only
after the hook exits 0. If `check-pre-merge` fails and a
`fix-pre-merge` hook is also present, `merge.rs` requires a clean
worktree outside `.factory/`, runs the fix hook, commits any
changes outside `.factory/`, reruns reviewers, reruns
`check-pre-merge`, copies updated run artifacts back to the source
run directory, and lands only when the recheck and reviewers pass.

The lower-level git mechanics remain in `worktree.rs`: copying run
artifacts, checking dirty worktrees, rebasing the run branch onto the
source branch, fast-forward merging, deleting the run branch, removing
the worktree, and setting status to `merged`.

### Cleanup

`cleanup.rs` owns cleanup of terminal legacy run artifacts and terminal
Work model state. Legacy cleanup selects `complete` and `merged` runs by
default, rejects non-terminal targets, and preserves the source run
directory. Applying legacy cleanup writes `cleaned.md` with the original
status, cleanup time, and worktree outcome. If a run records a
git-registered worktree, cleanup removes it through
`git worktree remove --force`; if the path is missing or is not a
registered worktree, cleanup records that outcome instead of deleting
arbitrary directories.

Work cleanup runs from the same `factory cleanup` command when no
`--run-id` is supplied. It selects Work Items only after every Attempt,
Task, and Merge Candidate is terminal, or after an operator explicitly
marks the Work Item abandoned with no executing or reviewing Attempts,
no executing Tasks, no reviewing Merge Candidates, and no executing
Merge Candidate merges. Applying cleanup removes the Work Item metadata
JSON, split Attempt records, split Task records, split Merge Candidate
records, referenced managed Work artifact files or directories, managed
candidate worktrees, and Work task branches. Managed artifact references
must be relative paths made only of normal path components and must
resolve under `.factory/work/artifacts/`; cleanup ignores absolute paths
and parent escapes. Managed Work worktrees are resolved with the same
expected workspace path rules used by Work task and merge execution, and
registered worktrees are removed through `git worktree remove --force`.
Missing worktree paths and unregistered
directories are reported without deleting arbitrary filesystem paths.
After planning stored Work Item cleanup, cleanup scans the top level of
`.factory/work/artifacts/` for directories whose names do not match any
stored Work Item JSON under `.factory/work/items/`. Dry runs report those
orphan Work artifact roots, and `--apply` removes only those top-level
artifact directories. File entries under `.factory/work/artifacts/` and
artifact roots for stored Work Items are ignored by orphan cleanup.

Cleanup resolves source Factory state even when invoked from a run
worktree by finding the registered worktree that points back to the
current checkout. That keeps cleanup state beside `.factory/runs/` in
the source repository instead of scattering cleanup markers into run
worktrees.

### Model selection environment

`coder.rs` owns model-selection environment variables. Claude uses
`FACTORY_CLAUDE_MODEL` first, falls back to `FACTORY_MODEL`, then uses
the built-in default `claude-opus-4-6`. Codex uses
`FACTORY_CODEX_MODEL` when set; otherwise Factory leaves Codex model
selection to the Codex CLI default. `FACTORY_CODER` selects the default
coder when the CLI does not pass `--coder`.

`FACTORY_CODEX_CA_BUNDLE` is not a model selector, but it lives beside
Codex launch configuration: for sandboxed Codex runs it overrides the
CA bundle path that Factory sets as `SSL_CERT_FILE`.

## Skills, expertise, and documentation

Three types of content serve different purposes. Procedures live in
`skills/` as step-by-step instructions an agent follows (following the
Agent Skills spec). Reference material for decision-making — principles,
patterns, conventions — lives in `expertise/` at the factory level and
in `.factory/expertise/` at the project level. System documentation
(`architecture.md`, `behaviors.md`) describes what IS: structure,
behaviors, and contracts.

Observations captured during usage become runs that build or improve
things. Patterns observed across runs accumulate as project expertise
in `.factory/expertise/`.

## Content resolution

`ContentResolver` resolves runtime content that the Factory binary reads
while executing commands. The implemented runtime content categories are
prompts under `prompts/` and sandbox profiles under `sandbox/`.

Runtime content uses a three-tier search chain. First match wins, no
merging:

1. **Project-local**: `<project>/.factory/<relative_path>`
2. **User config**: `~/.config/factory/<relative_path>`
3. **Bundled defaults**: compiled into the binary at build time

For example, a project can override the author prompt with
`<project>/.factory/prompts/author.md`, or a user can set a personal
default at `~/.config/factory/prompts/author.md`.

Skills and expertise are outside this resolver boundary. Agents read
skills from the repository or installed skill locations, and read
expertise from `expertise/`, `.factory/expertise/`, or skill
`references/` directories. Factory does not currently bundle or resolve
skills and expertise through `ContentResolver`.
