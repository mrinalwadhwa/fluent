---
name: plan-execution
description: >
  Break a designed approach into executable steps through discussion with
  the user. Prefer vertical slices, define verification points, identify
  scope trades, and produce plan.md — a living artifact that guides the
  executing agent and evolves during implementation.
---

# Plan Execution

Discuss the execution plan with the user step by step. Break the approach
into verifiable states the system reaches — not activities to perform.
Each step should deliver working behavior where possible. The plan guides
the executing agent but is expected to evolve during implementation.

---

## How to run this skill

### Phase 1 — Read the inputs

Read:
- The approved brief, behavior diff, and approach from the active
  planning conversation or draft artifacts — the normal source of intent
  before `factory work create` stores Work Item planning context
- Work Item planning context from `factory work show <work-item-id>` only
  when the Work Item already exists
- `.factory/runs/[run-id]/brief.md` only in a legacy fallback or
  recovery path
- `behaviors.diff.md` from the active planning context — what the system
  must do
- `approach.md` from the active planning context — how the system should
  do it
- `references/architecture.md` — architectural principles,
  especially the sections on simplicity and viewpoints, to inform
  step ordering and verification

Check: does this need a plan at all? A single-step bug fix or a
straightforward change that maps directly to one behavior doesn't need
a planning document. If the approach already describes a clear single
action, tell the user:

> "This looks straightforward enough to execute directly. Any reason
> to plan further, or should we proceed?"

If they agree, write a minimal plan. For Work-model planning, keep that
minimal plan as planning context for `factory work create`; set legacy
status to `planned` only in a legacy fallback or recovery path.

### Phase 2 — Assess scope

Determine whether the work can use one of these Work-model shapes:

- Work Item with one Attempt and one write Task
- peer Work Items that can proceed independently
- Work Item with one Attempt, plus likely follow-up Task notes for future
  execution

Indicators that decomposition is needed:
- The work spans multiple independent areas of the codebase
- The work would exceed a single session's context window
- Parts can proceed in parallel without knowing each other's results

Indicators that one Work Item with one write Task is fine:
- The work is focused on one area
- Everything depends on everything else
- The approach fits in one session

Share your assessment with the user:

> "I think this can be done as one Work Item with one Attempt and one
> write Task — the work is focused on [area]. Does that match your
> sense, or do you see independent pieces?"

Or:

> "I see two independent areas here: [X] and [Y]. They could become peer
> Work Items so each can have its own Attempt, Workspace, and Merge
> Candidate. Does it make sense to split them?"

### Phase 3 — Identify the first step

The first step matters most. Ideally it's a **walking skeleton** — the
thinnest possible end-to-end slice that proves the approach works.

> "The first thing I'd do is [thin slice]. It touches [layers/areas]
> and would prove [what it validates]. Does that seem like the right
> starting point?"

A walking skeleton catches integration problems early. If the first step
only builds one layer (just the database, just the API), problems hide
until integration — which is when they're most expensive.

### Phase 4 — Walk through remaining steps

Present steps one at a time or in small groups. For each step, discuss:

- **State reached** — what's true when this step is done? Phrase as an
  observable outcome, not an activity.
  - Not: "Implement auth endpoint"
  - Yes: "Users can log in via the API and receive a token"

- **Behaviors delivered** — which behaviors from behaviors.diff.md does
  this step satisfy?

- **Work unit** — does the state belong in the initial write Task, a
  likely follow-up Task note, or a peer Work Item? Treat Task sequencing
  as planning notes unless Factory can execute that dependency.

- **Verification** — how does the agent know the step is done? A test
  to run, an endpoint to hit, a file to check. Without this, the agent
  doesn't know when to move on.

- **Required or optional** — is this step essential, or could it be
  deferred if problems arise?

Present as a table for easy scanning:

> | Step | Work unit | State reached | Behaviors | Verification | Req? |
> |------|-----------|---------------|-----------|--------------|------|
> | 1 | Attempt: auth-work, Task: write | Users can log in via API | B1, B2 | `curl POST /login` returns 200 | Yes |
> | 2 | Likely follow-up Task note | Invalid credentials rejected | B3 | `curl` with bad password returns 401 | Yes |
> | 3 | Peer Work Item: login-hardening | Rate limiting on login | B4 | 10 rapid requests -> 429 | Optional |

Ask after each group:

> "Does this sequence make sense? Would you reorder anything or split
> any of these further?"

### Phase 5 — Progressive elaboration

If the full sequence isn't clear yet, that's fine. Plan what's visible
and mark the rest TBD:

> "I can see clearly through step 4. After that, it depends on what
> we learn from [X]. I'd leave the rest TBD and plan it when we get
> there."

Don't force false precision. The plan fills in during execution.

### Phase 6 — Scope trades and risks

Before finalizing, surface:

- **Scope trades** — which optional steps could be cut if the agent
  hits problems? Present in priority order (cut last to cut first).

- **Risks** — anything that might not work. If a step is risky, say so.
  The user can decide to proceed, investigate first, or adjust.

> "If things get tight, step 3 (rate limiting) is the first thing I'd
> defer — it's valuable but not blocking. The risk I see is [X] — if
> that doesn't work, we'd need to reconsider [Y]."

### Phase 7 — Sync points (parallel Work only)

When the plan has peer Work Items, identify where they must converge:

- Which Work Items, Attempts, Workspaces, or Merge Candidates are involved?
- What must be true at the convergence point?
- Is it blocking (one Work Item can't continue without it) or non-blocking?

> "After the API Work Item publishes its contract and the UI Work Item
> consumes it, they need to integrate. The API contract needs to be
> locked before the UI Work Item starts integration work."

Define interface contracts between peer Work Items before execution.
This prevents parallel work from diverging.

### Phase 8 — Assemble and confirm

Assemble `plan.md` and show the full plan:

> "Here's the complete plan. Does the full picture hold together?"

After the user approves the plan, create the Work Item or peer Work
Items with planning context stored directly in Work state. This is the
normal path for delegated Work execution. For a confirmed peer Work Item
plan, create each approved Work Item separately with its own brief,
behaviors, approach slice, plan slice, Attempt, Workspace, Merge
Candidate expectations, verification, and sync notes. Keep shared
sequencing as coordination notes outside the executable Work model; do
not collapse peer Work Items into one shared Attempt or Task sequence.

Pass the approved planning files in this order:

1. the approved brief
2. the approved behaviors diff
3. the approved approach
4. the approved plan

Use `factory work create --brief-file --behaviors-file --approach-file
--plan-file` so Factory stores the approved context on the Work Item and
derives write Task instructions from durable Work state. Create a legacy
run `execution-instructions.md` file only when a compatibility,
fallback, or recovery path still requires one. Do not write
`.factory/runs/[run-id]/brief.md`, `status`, or `.factory/active-run`
for ordinary Work-model planning when `factory work create` can express
the delegated execution.

Set legacy status to `planned` only when operating in a legacy fallback
or recovery path.

---

## Output format (Work Item planning)

```markdown
# Plan

Work Item: [work-item-id]
Brief: [one-line summary]
Attempt: [attempt-id or planned first attempt]

## Steps

| Step | Work unit | State reached | Behaviors | Verification | Req? |
|------|-----------|---------------|-----------|--------------|------|
| 1 | Attempt: [attempt-id], Task: write | [observable state] | [B1, B2] | [how to verify] | Yes |
| 2 | Likely follow-up Task note | [observable state] | [B3] | [how to verify] | Yes |
| 3 | Peer Work Item: [id] | [observable state] | [B4] | [how to verify] | Optional |
| 4+ | TBD — depends on [what] | | | | |

## Dependencies and sync points

- [Peer Work Item sync point, what must be true, blocking or not]
- [Attempt/Task sequencing note, if useful; do not present as an
  executable dependency unless Factory supports it]

## Scope trades

1. [Step N] — [why it's optional, what's lost if cut]

## Risks

- [Risk and what it affects]
```

## Output format (peer Work Items)

When the work decomposes into independent efforts, treat these as the
normal parallel planning vocabulary:

- peer Work Items with their own Attempts, Workspaces, and Merge
  Candidates

Use peer Work Items when each effort can be reviewed and merged
separately. If several pieces must share one candidate, keep them in one
Work Item and record likely follow-up Tasks or sequencing notes without
claiming Factory can pre-schedule Task dependencies.

Ask the user to confirm the split before writing the final plan.

```markdown
# Work Item planning

## Peer Work Items

### Work Item: [api-work-item-id]

[Brief for this Work Item — scope, behaviors it delivers, what it
produces.]

Attempt: [attempt-id]
Workspace: [workspace expectation]
Merge Candidate: [candidate expectation and merging checks]

- Initial write Task: [scope and output]
- Likely follow-up Task note: [scope and output, if the first Task
  reveals it is needed]

### Work Item: [ui-work-item-id]

[Brief for this Work Item.]

Attempt: [attempt-id]
Workspace: [workspace expectation]
Merge Candidate: [candidate expectation and merging checks]

- Initial write Task: [scope and output]
- Likely follow-up Task note: [scope and output, if the first Task
  reveals it is needed]

## Sync points

- [Which peer Work Items converge, what must be true, blocking
  or not]

## Interfaces

- [Contract between parallel work — shared types, API shape, file paths]
```

## Legacy fallback format (parallel child runs)

Use the legacy group/step format only when the Work model cannot yet
express the required coordination, such as coordinated child-run
decomposition for one large effort or an explicit recovery path. In that
case, the factory parses the structure and creates child runs
automatically. Each H2 is a group executed in sequence. Each H3 is a
step. Groups marked `(parallel)` launch their steps concurrently; unmarked
groups run steps one at a time. Child run IDs are
`{parent-id}-{group-idx}-{step-idx}` (1-indexed).

---

## Rules

- **Interview, don't present.** Walk through steps with the user, one
  at a time or in small groups. Don't produce a finished plan and ask
  for approval.
- **States, not activities.** Each step describes a state the system
  reaches — observable, testable. Push activity phrasing ("work on X")
  into state phrasing ("X works").
- **Vertical slices over horizontal layers.** Each step should deliver
  working end-to-end behavior where possible. Avoid building all of one
  layer before touching the next.
- **Walking skeleton first.** The first step should prove the approach
  works end-to-end, even if the functionality is minimal.
- **Every behavior must have a home.** Each behavior in behaviors.diff.md
  maps to a step, Attempt, Task note, or peer Work Item. If a behavior
  has no home, the plan is incomplete.
- **Verification is required.** Every step needs a way for the agent to
  confirm it's done. No verification → the agent can't know when to
  move on.
- **Progressive elaboration is correct.** Plan what you can see. Mark
  the rest TBD. The plan evolves during execution.
- **Classify scope early.** Mark steps as required or optional from the
  start. This makes scope trading possible when problems arise.
- **Decompose by scope, not by chore.** Use Work Items and Task notes to
  capture coherent areas of behavior, not checklist activities. "Auth
  system" is a good Work Item. "Write tests" is not.
- **Interfaces before execution.** When peer Work Items produce code that
  must integrate, define the contract in the plan.
- **Don't over-plan.** The plan breaks the approach into steps. It does
  not redesign the approach. If planning reveals the approach is wrong,
  go back to design-approach.
- **The plan is a living artifact.** The executing agent may discover
  steps need adjustment. They can adapt within the approach, or propose
  changes via `needs-user` if significant.
- **Trivial work needs trivial plans.** Don't force a planning document
  on a one-step fix.
