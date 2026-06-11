---
name: build-in-the-factory
description: >
  Operate the factory workflow to build software autonomously over extended
  periods. Covers the full lifecycle: brief, behaviors, approach, plan,
  execution, and review. Teaches which stages are interactive and which are
  autonomous, how to use the Work model, and when to pause or
  renegotiate rather than drift.
---

# Build in the Factory

Follow a structured workflow: capture intent, define behaviors, design an
approach, plan execution, execute, and review. Some stages need the user;
others run autonomously.

---

## Core principle

**Behaviors are the contract. Approach is direction. Both can be
renegotiated.**

The defined behaviors describe what the system must do. The approach
describes how. During execution, if you discover the approach doesn't
work, adapt it — or propose a change via `needs-user` if the change is
significant. If you discover a behavior is wrong or incomplete, pause
and renegotiate rather than deliver the wrong thing.

The cost of pausing to renegotiate is low. The cost of delivering the
wrong thing is high.

---

## Workflow

```
Capture    Define      Design     Plan       Execute   Review
Brief  →  Behaviors →  Approach →  Plan  →  Execute →  Review
(interactive)                               (autonomous)
```

The normal delegated build lifecycle is the Work model:
Work Item → Attempt → Task → Workspace → Merge Candidate. Work Items
represent planned Factory work, Attempts carry one execution history,
Tasks are schedulable units, Workspaces are the filesystem contexts
Tasks read or write, and Merge Candidates are reviewed outputs ready to
land.

The older `.factory/runs/[run-id]/` lifecycle still exists only as
legacy compatibility. Use Work-model commands for new delegated
execution. Use legacy run artifacts only for explicit fallback,
Fargate-only execution, coordinated child-run decomposition, or recovery
of existing run state.

---

## On session start

Check Work Items first, then legacy runs:

**Work Items:** Run `factory status` or `factory work list`. If stored
Work Items exist, inspect the relevant item with `factory work show
<work-item-id>`. Continue the latest non-terminal Attempt when the next
action is clear, or present the `needs-user` handoff when an Attempt or
Merge Candidate asks for user input.

**Merge Candidates:** If `factory status` shows a pending Merge
Candidate, inspect it with `factory work merge-candidate <work-item-id>
<merge-candidate-id>`. Land it with `factory work merge <work-item-id>
<merge-candidate-id>` after the user accepts the candidate or the run
policy says autonomous merging is allowed.

Then check `.factory/runs/` only for legacy compatibility or recovery
runs that need attention:

**Completed runs with reports:** Scan for runs with status `complete`
that have a `report.md` but no `reported` marker. These completed
while the user was away. Offer to walk through them:

> "Run [id] completed ([brief summary]). Want me to walk through
> what happened?"

If the user says yes, read `report.md` for the summary, then drill
into the underlying artifacts as needed — review findings, git diff,
session transcripts. Present the key points in small pieces. When
the user is satisfied, write a `reported` marker to the run directory
so it's not offered again.

If multiple runs completed, list them and let the user pick which to
review first.

**Active run:** Read `.factory/active-run` for the current run-id.
Check the status:

- `executing` — read `handoff.md` and continue from where the previous
  session left off. Do not re-read the full history.
- `reviewing` — reviewers are running autonomously. Note the run status
  to the user and wait. Do not treat it as idle.
- `needs-user` — present the question from `handoff.md` and wait for the
  user's answer.

**No Work Items or runs needing attention** — ask the user what they
want to build.

---

## Interactive stages (user present)

Follow the corresponding skill directly in your session. The user is
present for conversation and review.

### 1. Capture brief

Follow the `capture-brief` skill. Interview the user to capture their
intent. Read the codebase for context. Write a brief that will become
Work Item planning context. Write legacy `brief.md` and set legacy
status to `briefed` only for fallback or recovery paths.

**Review-only work:** If the user wants a full-codebase review (not
building something new), capture enough context to create a Work Item.
Use the Work-model review-only flow in the autonomous stages below.

### 2. Define behaviors

Follow the `define-behaviors` skill. Read the brief and existing
behaviors. Elaborate into EARS-format behavioral statements. Write
`behaviors.diff.md`. Present to the user for critical review. On
approval, keep the diff as approved planning context for the Work Item.
Set legacy status to `behaviors-defined` only for fallback or recovery
paths.

### 3. Design approach

Follow the `design-approach` skill. Research external systems, evaluate
options, make technical choices, and record the expertise files that
should guide execution. Write `approach.md`. Present to the user for
review. On approval, keep it as approved planning context for the Work
Item. Set legacy status to `approach-designed` only for fallback or
recovery paths.

### 4. Plan execution

Follow the `plan-execution` skill. Break the approach into executable
steps. Determine whether the work should stay in one Attempt, split
into separate peer Work Items, or use legacy child-run decomposition as
a fallback for one large effort. Write `plan.md`. Present to the user
for review. On approval, create the Work Item with the approved planning
files and set legacy status to `planned` only for fallback or recovery
paths.

---

## Autonomous stages (user away)

Once the plan is approved, use the Work model for delegated execution:

1. Create a Work Item with the approved planning files:
   `factory work create <work-item-id> --title <title>
   --brief-file <brief.md> --behaviors-file <behaviors.diff.md>
   --approach-file <approach.md> --plan-file <plan.md>`.
2. Create an Attempt: `factory work attempt <work-item-id>
   <attempt-id>`.
3. Run the Attempt: `factory work attempt run <work-item-id>
   <attempt-id>`.
4. Inspect status with `factory status` or `factory work show
   <work-item-id>`.
5. When the Attempt creates a Merge Candidate, inspect it with
   `factory work merge-candidate <work-item-id> <merge-candidate-id>`.
6. Land through `factory work merge <work-item-id>
   <merge-candidate-id>`.

`factory work attempt run` advances the next safe transition by running
planned write and review Tasks through the existing Task executor. It
reloads stored Work Item state between transitions, carries Work Item
instructions or planning context into initial and follow-up write Tasks,
creates follow-up write Tasks after failed reviews, and records
`needs-user` when the review state cannot be resolved autonomously.

For unrelated work that can proceed in parallel, create independent peer
Work Items rather than decomposing one parent run. Use the group/step
plan format only when one large effort needs coordinated child work with
explicit sync points and the Work model cannot yet carry that
decomposition. In that case, the legacy `factory run` fallback detects
the structured `plan.md`, creates child run directories and worktrees,
and launches child sessions.

For full-codebase review-only work, use the Work model by creating a
Work Item, running `factory work review-codebase <work-item-id>
<attempt-id>`, then running `factory work attempt run <work-item-id>
<attempt-id>`. Use legacy `factory run` only for compatibility,
Fargate-only execution, coordinated child-run decomposition for one large
effort, or recovery of existing `.factory/runs` state. The fallback
still manages the session loop by restarting agents across sessions as
long as work remains.

### 5. Execute

Implement the approved Task in the assigned Workspace. In the Work model,
write Tasks make commits in their writable Workspace, review Tasks write
Work artifacts, and the Attempt loop schedules follow-up Tasks or creates
a Merge Candidate from the latest accepted output. Treat legacy
`.factory/runs` execution as fallback or recovery state; do not describe
new Work-model Tasks as automatically creating legacy child runs. At leaf
level, write code directly.

During execution, you have latitude to adapt within the approach. For
significant deviations, pause and renegotiate via `needs-user`.

### 6. Review

Reviewers evaluate your output. In the Work model, review Tasks write
artifacts under `.factory/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`, and
Attempt review state decides whether to create a Merge Candidate,
schedule follow-up write Tasks, or ask the user. In the legacy fallback,
review results go in the run's `reviews/` directory. Verdicts: pass
(done), fail (revise), uncertain (ask user).

---

## When to pause

Pause and set status to `needs-user` when:
- You are genuinely uncertain about intent, approach, or scope
- You discover a defined behavior is wrong or incomplete
- You need to deviate significantly from the approach
- A reviewer returns `uncertain`
- You encounter a decision with significant consequences that could go
  multiple ways
- You need access, credentials, or information you don't have

Do NOT pause for:
- Decisions you can make confidently from context
- Minor implementation choices within the approach
- Things you can verify by reading the code or running tests

---

## Work state

Durable Work model state lives under `.factory/work/`:

| Path | Purpose |
|---|---|
| `.factory/work/items/<work-item-id>.json` | Stored Work Item metadata and planning context |
| `.factory/work/attempts/<work-item-id>/<attempt-id>.json` | Stored Attempt records |
| `.factory/work/tasks/<work-item-id>/<attempt-id>/<task-id>.json` | Stored Task records |
| `.factory/work/merge-candidates/<work-item-id>/<candidate-id>.json` | Stored Merge Candidate records |
| `.factory/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/` | Task artifacts such as review output |
| `.factory/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/` | Merge-time review and execution artifacts |

Managed candidate worktrees live beside the source checkout as
`../work-<work-item-id-byte-len>-<work-item-id>-<attempt-id>`.

Use `factory work show <work-item-id>` for the durable object. Use
`factory status` or `factory dashboard` for operator-facing summaries.
Use `factory cleanup` for a dry-run cleanup report after terminal Work
Items land or fail; add `--apply` to remove terminal Work Item state,
referenced artifacts, managed candidate worktrees, and Work branches.
Cleanup skips active Attempts, Tasks, and Merge Candidates.

## Legacy run state

Legacy run state is compatibility and recovery state. It lives in
`.factory/runs/[run-id]/`:

| File | Purpose |
|---|---|
| `brief.md` | User's intent (from capture-brief) |
| `behaviors.diff.md` | New behaviors this run adds (from define-behaviors) |
| `approach.md` | Solution direction (from design-approach) |
| `plan.md` | Execution steps (from plan-execution) |
| `status` | `briefed`, `behaviors-defined`, `approach-designed`, `planned`, `executing`, `reviewing`, `rate-limited`, `needs-user`, `complete`, `failed`, `merged` |
| `handoff.md` | Context for the next session |
| `active-run` | Current run-id (in `.factory/`) |
| `source-branch` | Branch the run forked from |
| `worktree` | Path to the run's git worktree |
| `runtime` | `local` or `fargate` |
| `coder` | `claude` or `codex` |
| `handle` | Runtime-specific identifier |
| `mode` | `review` or absent (defaults to full lifecycle) |
| `reviewers` | Comma-separated reviewer filter (optional) |
| `scope` | Review focus targeting (optional) |
| `sessions/` | Per-session transcript directories |
| `sessions.log` | Per-session metadata log |
| `report.md` | Generated run report |
| `cleaned.md` | Cleanup context for complete or merged runs cleaned by `factory cleanup` |
| `reviews/` | Review artifacts |
| `children` | Child run IDs, one per line (parallel runs only, written by the factory) |
| `parent` | Parent run ID (child runs only, written by the factory) |

These files are not the normal planning handoff for Work-model
execution. Create or update them only when using legacy `factory run`,
coordinated child-run decomposition, Fargate-only execution, or explicit
recovery of existing run state.

`factory cleanup` also handles complete or merged legacy runs. It
defaults to a dry run and requires `--apply` before removing registered
run worktrees or writing cleanup markers.

Each legacy run executes in its own git worktree (a sibling of the
source worktree). The factory command creates the worktree at launch
time. When done, `factory merge` rebases the worktree branch onto the
source branch, fast-forward merges, captures artifacts, and removes the
worktree.

---

## Factory commands

Work-model commands are listed first because they are the normal path for
new delegated work. Legacy run commands follow as compatibility,
Fargate, and recovery commands while the old session loop remains
available.

```sh
factory work create <id> --title <t> # create a stored Work Item
factory work create <id> --title <t> --planning-context-file <path> # load planning context
factory work create <id> --title <t> --brief-file <b> --behaviors-file <beh> --approach-file <a> --plan-file <p> # store approved planning files
factory work create <id> --title <t> --instructions <text> # store prompt text
factory work create <id> --title <t> --instructions-file <path> # load prompt file
factory work list                    # list stored Work Items
factory work show <id>               # show one Work Item as JSON
factory work abandon <id> --reason <text> # mark a stale Work Item abandoned
factory work attempt <id> <attempt>  # add an Attempt with a write Task
factory work attempt run <id> <attempt> # advance an Attempt
factory work review <id> <attempt>   # plan review Tasks
factory work review-codebase <id> <attempt> # add a review-only Attempt
factory work task run <id> <attempt> <task> # run one Task
factory work merge-candidate <id> <candidate> # show a Merge Candidate
factory work merge <id> <candidate>  # execute a Merge Candidate
factory status                       # show Work Items by default
factory dashboard                    # open the live dashboard
factory cleanup                      # dry-run stale Work and legacy cleanup
factory cleanup --apply              # clean terminal Work state and legacy runs

factory status --runs                # show legacy Runs compatibility view
factory run                          # fallback legacy session loop
factory run --run-id <id>            # target a legacy run
factory run --coder codex            # run legacy path with Codex
factory run --runtime fargate        # run legacy path on Fargate
factory summary                      # summarize one legacy run
factory watch                        # poll status, notify on change
factory review                       # create or reuse a legacy review run
factory pull                         # download legacy workspace from S3
factory shell                        # shell into a legacy remote task
factory resume                       # restart a paused legacy run
factory merge                         # land a completed legacy run
factory init                         # initialize .factory/ directories
factory version                      # print version and build commit
```

For interactive stages, do not call these commands. Follow the skills
directly in your session.

---

## Gotchas

- When resuming a run, read `handoff.md` only — not the full history.
  The handoff contains everything you need to continue. Re-reading the
  full run history wastes context and risks confusion from stale state.

- Never call `factory run` from within an interactive session. The
  legacy command launches a session loop that manages your process.
  Calling it from inside a session creates a nested loop. If you need
  the legacy fallback, tell the user to run it from their terminal.

- Do not default to `.factory/runs` for Work execution planning. Store
  approved planning context on the Work Item and treat legacy run files
  as bridge context only when the Work model lacks the capability you
  need.
