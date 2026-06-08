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
- Work Item context from `factory work show <work-item-id>`, or
  `.factory/runs/[run-id]/brief.md` in the legacy fallback — the intent
- `behaviors.diff.md` from the active planning context — what the system
  must do
- `approach.md` from the active planning context — how the system should
  do it
- `expertise/architecture.md` — architectural principles,
  especially the sections on simplicity and viewpoints, to inform
  step ordering and verification

Check: does this need a plan at all? A single-step bug fix or a
straightforward change that maps directly to one behavior doesn't need
a planning document. If the approach already describes a clear single
action, tell the user:

> "This looks straightforward enough to execute directly. Any reason
> to plan further, or should we proceed?"

If they agree, write a minimal plan and set status to `planned`.

### Phase 2 — Assess scope

Determine whether the work can be executed as a single run (leaf) or
needs decomposition into child runs.

Indicators that decomposition is needed:
- The work spans multiple independent areas of the codebase
- The work would exceed a single session's context window
- Parts can proceed in parallel without knowing each other's results

Indicators that direct execution is fine:
- The work is focused on one area
- Everything depends on everything else
- The approach fits in one session

Share your assessment with the user:

> "I think this can be done in a single run — the work is focused on
> [area]. Does that match your sense, or do you see independent pieces?"

Or:

> "I see two independent areas here: [X] and [Y]. They could run in
> parallel. Does it make sense to split them?"

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

- **Verification** — how does the agent know the step is done? A test
  to run, an endpoint to hit, a file to check. Without this, the agent
  doesn't know when to move on.

- **Required or optional** — is this step essential, or could it be
  deferred if problems arise?

Present as a table for easy scanning:

> | Step | State reached | Behaviors | Verification | Req? |
> |------|--------------|-----------|-------------|------|
> | 1 | Users can log in via API | B1, B2 | `curl POST /login` returns 200 | Yes |
> | 2 | Invalid credentials rejected | B3 | `curl` with bad password returns 401 | Yes |
> | 3 | Rate limiting on login | B4 | 10 rapid requests → 429 | Optional |

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

### Phase 7 — Sync points (parallel plans only)

When the plan has parallel child runs, identify where they must converge:

- Which child runs are involved?
- What must be true at the convergence point?
- Is it blocking (one child run can't continue without it) or non-blocking?

> "After child run A finishes the API and child run B finishes the UI,
> they need to integrate. The API contract needs to be locked before B
> can start integration work."

Define interface contracts between child runs before execution — this
prevents parallel child runs from diverging.

### Phase 8 — Assemble and confirm

Assemble `plan.md` and show the full plan:

> "Here's the complete plan. Does the full picture hold together?"

Set status to `planned`.

---

## Output format (leaf run)

```markdown
# Plan

Run: [run-id]
Brief: [one-line summary]

## Steps

| Step | State reached | Behaviors | Verification | Req? |
|------|--------------|-----------|-------------|------|
| 1 | [observable state] | [B1, B2] | [how to verify] | Yes |
| 2 | [observable state] | [B3] | [how to verify] | Yes |
| 3 | [observable state] | [B4] | [how to verify] | Optional |
| 4+ | TBD — depends on [what] | | | |

## Scope trades

1. [Step N] — [why it's optional, what's lost if cut]

## Risks

- [Risk and what it affects]
```

## Output format (parallel plan)

When the work decomposes into independent child runs, use the group/step
format. The factory parses this structure and creates child runs
automatically. Each H2 is a group (executed in sequence). Each H3 is a
step. Groups marked `(parallel)` launch their steps concurrently; unmarked
groups run steps one at a time.

Child run IDs are `{parent-id}-{group-idx}-{step-idx}` (1-indexed).

```markdown
## Group 1 (parallel)

### Step: [step title]

[Brief for this child run — scope, behaviors it delivers, what it produces.]

### Step: [step title]

[Brief for this child run.]

## Group 2

### Step: [step title]

[Brief — this runs after group 1 merges.]

## Sync points

- [Which child runs converge, what must be true, blocking or not]

## Interfaces

- [Contract between parallel child runs — shared types, API shape, file paths]
```

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
  maps to a step or child run. If a behavior has no home, the plan is
  incomplete.
- **Verification is required.** Every step needs a way for the agent to
  confirm it's done. No verification → the agent can't know when to
  move on.
- **Progressive elaboration is correct.** Plan what you can see. Mark
  the rest TBD. The plan evolves during execution.
- **Classify scope early.** Mark steps as required or optional from the
  start. This makes scope trading possible when problems arise.
- **Decompose by scope, not by task.** Child runs own areas of the
  codebase, not individual tasks. "Auth system" is a good child run.
  "Write tests" is not.
- **Interfaces before execution.** When parallel child runs produce code
  that must integrate, define the contract in the plan.
- **Don't over-plan.** The plan breaks the approach into steps. It does
  not redesign the approach. If planning reveals the approach is wrong,
  go back to design-approach.
- **The plan is a living artifact.** The executing agent may discover
  steps need adjustment. They can adapt within the approach, or propose
  changes via `needs-user` if significant.
- **Trivial work needs trivial plans.** Don't force a planning document
  on a one-step fix.
