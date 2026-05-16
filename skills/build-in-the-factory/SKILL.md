---
name: build-in-the-factory
description: >
  Operate the factory workflow to build software autonomously over extended
  periods. Covers the full lifecycle: brief, behaviors, approach, plan,
  execution, and review. Teaches which stages are interactive and which are
  autonomous, how to read and write run state, and when to pause or
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

Each stage produces an artifact in `.factory/runs/[run-id]/`. The
artifacts are shared context between you and the user, and between
sessions.

---

## On session start

Check `.factory/runs/` for runs that need attention:

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
- `needs-user` — present the question from `handoff.md` and wait for the
  user's answer.

**No runs needing attention** — ask the user what they want to build.

---

## Interactive stages (user present)

Follow the corresponding skill directly in your session. The user is
present for conversation and review.

### 1. Capture brief

Follow the `capture-brief` skill. Interview the user to capture their
intent. Read the codebase for context. Write `brief.md`. Set status to
`briefed`.

**Review runs:** If the user wants a full-codebase review (not building
something new), this is a lightweight run. Write the brief, set the
mode to `review`, and skip directly to `planned`. See the capture-brief
skill for details.

### 2. Define behaviors

Follow the `define-behaviors` skill. Read the brief and existing
behaviors. Elaborate into EARS-format behavioral statements. Write
`behaviors.diff.md`. Present to the user for critical review. On
approval, set status to `behaviors-defined`.

### 3. Design approach

Follow the `design-approach` skill. Research external systems, evaluate
options, make technical choices. Write `approach.md`. Present to the
user for review. On approval, set status to `approach-designed`.

### 4. Plan execution

Follow the `plan-execution` skill. Break the approach into executable
steps. Determine if decomposition into child runs is needed. Write
`plan.md`. Present to the user for review. On approval, set status to
`planned`.

---

## Autonomous stages (user away)

Once the plan is approved, the user may invoke `factory run` and walk
away. The factory command manages the session loop — restarting you across
sessions as long as work remains. Write the status and handoff files;
the loop handles the rest.

### 5. Execute

Implement the plan. If the plan decomposes into child runs, create their
run directories and execute them. At leaf level, write code directly.

During execution, you have latitude to adapt within the approach. For
significant deviations, pause and renegotiate via `needs-user`.

### 6. Review

Reviewers evaluate your output. Review results go in the `reviews/`
directory of the run. Verdicts: pass (done), fail (revise), uncertain
(ask user).

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

## Run state

Run state lives in `.factory/runs/[run-id]/`:

| File | Purpose |
|---|---|
| `brief.md` | User's intent (from capture-brief) |
| `behaviors.diff.md` | New behaviors this run adds (from define-behaviors) |
| `approach.md` | Solution direction (from design-approach) |
| `plan.md` | Execution steps (from plan-execution) |
| `status` | `briefed`, `behaviors-defined`, `approach-designed`, `planned`, `executing`, `rate-limited`, `needs-user`, `complete`, `failed`, `landed` |
| `handoff.md` | Context for the next session |
| `active-run` | Current run-id (in `.factory/`) |
| `source-branch` | Branch the run forked from |
| `worktree` | Path to the run's git worktree |
| `runtime` | `local` or `fargate` |
| `handle` | Runtime-specific identifier |
| `mode` | `review` or absent (defaults to full lifecycle) |
| `reviewers` | Comma-separated reviewer filter (optional) |
| `scope` | Review focus targeting (optional) |
| `sessions/` | Per-session transcript directories |
| `sessions.log` | Per-session metadata log |
| `report.md` | Generated run report |
| `reviews/` | Review artifacts |

Each run executes in its own git worktree (a sibling of the source
worktree). The factory command creates the worktree at launch time.
When done, `factory land` rebases the worktree branch onto the source
branch, fast-forward merges, captures artifacts, and removes the
worktree.

---

## Factory commands

```sh
factory run                          # start the local session loop
factory run --run-id <id>            # target a specific run
factory run --runtime fargate        # run on Fargate
factory status                       # show all runs and their state
factory watch                        # poll status, notify on change
factory pull                         # download completed workspace from S3
factory shell                        # interactive shell into running task
factory resume                       # restart a paused run
```

For interactive stages, do not call these commands. Follow the skills
directly in your session.

---

## Gotchas

- When resuming a run, read `handoff.md` only — not the full history.
  The handoff contains everything you need to continue. Re-reading the
  full run history wastes context and risks confusion from stale state.

- Never call `factory run` from within an interactive session. The
  factory command launches a session loop that manages your process.
  Calling it from inside a session creates a nested loop. If you need
  to start a run, tell the user to run it from their terminal.
