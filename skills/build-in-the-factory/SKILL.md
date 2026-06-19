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

The delegated build lifecycle is the Work model:
Work Item → Attempt → Task → Workspace → Merge Candidate. Work Items
represent planned Factory work, Attempts carry one execution history,
Tasks are schedulable units, Workspaces are the filesystem contexts
Tasks read or write, and Merge Candidates are reviewed outputs ready to
land.

---

## On session start

Check Work Items:

**Work Items:** Run `factory status` or `factory work list`. If stored
Work Items exist, inspect the relevant item with `factory work show
<work-item-id>`. Continue the latest non-terminal Attempt when the next
action is clear, or present the `needs-user` handoff when an Attempt or
Merge Candidate asks for user input.

**Merge Candidates:** If `factory status` shows a pending Merge
Candidate, inspect it with `factory work merge-candidate <work-item-id>
<merge-candidate-id>`. Land it with `factory work merge <work-item-id>`
after the user accepts the candidate or the policy says autonomous
merging is allowed.

**No Work Items needing attention** — ask the user what they want to
build.

---

## Interactive stages (user present)

Follow the corresponding skill directly in your session. The user is
present for conversation and review.

### 1. Capture brief

Follow the `capture-brief` skill. Interview the user to capture their
intent. Read the codebase for context. Write a brief that will become
Work Item planning context.

**Review-only work:** If the user wants a full-codebase review (not
building something new), capture enough context to create a Work Item.
Use the review-only flow in the autonomous stages below.

### 2. Define behaviors

Follow the `define-behaviors` skill. Read the brief and existing
behaviors. Elaborate into EARS-format behavioral statements. Write
`behaviors.diff.md`. Present to the user for critical review. On
approval, keep the diff as approved planning context for the Work Item.

### 3. Design approach

Follow the `design-approach` skill. Research external systems, evaluate
options, make technical choices, and record the expertise files that
should guide execution. Write `approach.md`. Present to the user for
review. On approval, keep it as approved planning context for the Work
Item.

### 4. Plan execution

Follow the `plan-execution` skill. Break the approach into executable
steps. Determine whether the work should stay in one Attempt or split into
separate peer Work Items. Write `plan.md`. Present to the user for
review. On approval, create the Work Item with the approved planning
files.

---

## Autonomous stages (user away)

Once the plan is approved, use the Work model for delegated execution:

1. Create a Work Item with the approved planning files:
   `factory work create <work-item-id> --title <title>
   --brief-file <brief.md> --behaviors-file <behaviors.diff.md>
   --approach-file <approach.md> --plan-file <plan.md>`.
2. Create an Attempt: `factory work attempt <work-item-id>`.
   (An `attempt-N` id is auto-assigned; pass an explicit id for
   scripted flows.)
3. Run the Attempt: `factory work attempt run <work-item-id>`.
   (Defaults to the most recently created Attempt; pass an explicit
   id to target a specific one.)
4. Inspect status with `factory status` or `factory work show
   <work-item-id>`.
5. When the Attempt creates a Merge Candidate, inspect it with
   `factory work merge-candidate <work-item-id> <merge-candidate-id>`.
6. Land through `factory work merge <work-item-id>`.
   (Defaults to the most recently created Merge Candidate; pass an
   explicit id to target a specific one.)

`factory work attempt run` advances the next safe transition by running
planned write and review Tasks through the existing Task executor. It
reloads stored Work Item state between transitions, carries Work Item
instructions or planning context into initial and follow-up write Tasks,
creates follow-up write Tasks after failed reviews, and records
`needs-user` when the review state cannot be resolved autonomously.

For unrelated work that can proceed in parallel, create independent peer
Work Items.

For full-codebase review-only work, use the Work model by creating a
Work Item, running `factory work review-codebase <work-item-id>
<attempt-id>`, then running `factory work attempt run <work-item-id>
<attempt-id>`.

### Writer testing contract

The writer owns producing tests alongside code. When committing a
candidate:

- `.factory/tester.yaml` declares the project's test commands (one entry
  per harness, e.g., Rust nextest + shell).
- Each EARS statement has either a `Test:` reference pointing at a real
  test or an `Untestable:` marker with a one-line reason.
- Run the project's tests before committing (best practice, not enforced).

The Tester Task is the safety net — it runs after the write completes
and produces `tester-results.json` for all reviewers.

### 5. Execute

Implement the approved Task in the assigned Workspace. In the Work model,
write Tasks make commits in their writable Workspace, review Tasks write
Work artifacts, and the Attempt loop schedules follow-up Tasks or creates
a Merge Candidate from the latest accepted output. At leaf level, write
code directly.

During execution, you have latitude to adapt within the approach. For
significant deviations, pause and renegotiate via `needs-user`.

### 6. Review

Reviewers evaluate your output. In the Work model, review Tasks write
artifacts under `.factory/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`, and
Attempt review state decides whether to create a Merge Candidate,
schedule follow-up write Tasks, or ask the user. Verdicts: pass (done),
fail (revise), uncertain (ask user).

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

---

## Factory commands

```sh
factory work create <id> --title <t> # create a stored Work Item
factory work create <id> --title <t> --planning-context-file <path> # load planning context
factory work create <id> --title <t> --brief-file <b> --behaviors-file <beh> --approach-file <a> --plan-file <p> # store approved planning files
factory work create <id> --title <t> --instructions <text> # store prompt text
factory work create <id> --title <t> --instructions-file <path> # load prompt file
factory work list                    # list stored Work Items
factory work show <id>               # show one Work Item as JSON
factory work abandon <id> --reason <text> # mark a stale Work Item abandoned
factory work attempt <id> [<attempt>] # add an Attempt (auto-assigns attempt-N)
factory work attempt run <id> [<attempt>] # advance an Attempt (defaults to latest)
factory work review <id> <attempt>   # plan review Tasks
factory work review-codebase <id> <attempt> # add a review-only Attempt
factory work task run <id> <attempt> <task> # run one Task
factory work merge-candidate <id> <candidate> # show a Merge Candidate
factory work merge <id> [<candidate>] # execute a Merge Candidate (defaults to latest)
factory status                       # show Work Items by default
factory dashboard                    # open the live dashboard
factory cleanup                      # dry-run stale Work cleanup
factory cleanup --apply              # clean terminal Work state
factory init                         # initialize .factory/ directories
factory version                      # print version and build commit
```

For interactive stages, do not call these commands. Follow the skills
directly in your session.

---

## Gotchas

- Store approved planning context on the Work Item. Do not create
  separate planning files outside durable Work state.
