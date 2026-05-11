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
│  review-architecture                            │
│  Portable procedures any agent follows          │
├─────────────────────────────────────────────────┤
│  build-in-the-factory skill                     │
│  Teaches agents the full workflow               │
├─────────────────────────────────────────────────┤
│  Factory command                                │
│  factory run / status / pull / shell / watch    │
│  factory resume                                 │
│  Deterministic, operational                     │
└─────────────────────────────────────────────────┘
```

**Skills** describe procedures. They don't know about sandboxes, sessions,
or backends.

**The factory command** handles the operational envelope: sandbox setup,
credential injection, session continuity, worktree creation, and remote
execution.

**The build-in-the-factory skill** bridges the two. An agent reads it and
can drive the entire workflow.

## Workflow

```
Brief → Behaviors → Approach → Plan → Execute → Review
(interactive)                         (autonomous)
```

Interactive stages happen in the agent's session with the user present.
The agent follows skills directly.

Autonomous stages don't need the user. The factory command manages the
session loop and can run locally or on Fargate.

## The run

The core recursive unit of work.

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

When done, the user reviews the branch diff, merges into the source
branch, and removes the worktree.

### Run state

| File | Purpose |
|---|---|
| `brief.md` | User's intent |
| `behaviors.diff.md` | New behaviors this run adds |
| `approach.md` | Solution direction |
| `plan.md` | Execution steps |
| `status` | `briefed`, `behaviors-defined`, `approach-designed`, `planned`, `executing`, `rate-limited`, `needs-user`, `complete`, `failed` |
| `handoff.md` | Context for the next session |
| `active-run` | Current run-id (in `.factory/`) |
| `source-branch` | Branch the run forked from |
| `worktree` | Path to the run's worktree |
| `backend` | `local` or `fargate` |
| `handle` | Backend-specific identifier |
| `mode` | `build` (default) or `review` |
| `reviewers` | Comma-separated reviewer filter (optional) |
| `scope` | Review focus targeting (optional) |
| `reviews/` | Directory for review artifacts |

### Run-id resolution

The factory command resolves the run-id through a priority chain:

1. `--run-id` flag
2. `FACTORY_RUN_ID` environment variable
3. `.factory/active-run` pointer file
4. Scan `.factory/runs/` for active status (fallback)

### Session continuity

The factory command runs a session loop:

```
while run is not complete:
    launch agent with -p and brief/handoff prompt
    agent works until context exhaustion or completion
    agent writes handoff.md + status file
    capture session snapshot
    if terminal status: stop
    if executing: restart
    if rate-limited: wait 5 minutes, restart
```

The agent writes one word to `status` before exiting. The loop reads that
word. That's the entire contract.

### Session snapshots

Captured at each session boundary:

```
.factory/runs/[run-id]/sessions/
  session-1/
    transcript.jsonl
    memory/
    todos/
    plans/
  session-2/
    ...
```

First-class learning artifacts, not debug logs.

## Agents

### Author

Implements code. Follows the plan. Pauses when genuinely uncertain rather
than drifting.

### Reviewers

Evaluate the author's output. Three reviewers:

**Documentation reviewer** (code-aware): reads code and docs, checks
accuracy, writing quality, and completeness
(`skills/review-documentation/SKILL.md`).

**Behavior reviewer** (user-facing): observes behavior only, cannot see
code — evaluates the system from the outside, as a user would
(`skills/review-behaviors/SKILL.md`).

**Architecture reviewer** (code-aware): reads code and architectural
expertise, evaluates structural decisions against principles
(`skills/review-architecture/SKILL.md`).

Review verdicts: **pass** / **uncertain** (ask user) / **fail** (send
back to author with findings).

When the author receives findings from multiple reviewers, it weighs
each finding according to the reviewer's domain expertise. When reviewers
disagree, the one with relevant expertise for that finding takes priority.
The author escalates to `needs-user` only when genuinely stuck.

### Review phase

The session loop triggers a review phase when the author sets status to
`complete`. Reviewers run in parallel, each producing an artifact in
`.factory/runs/[run-id]/reviews/`. The loop parses each reviewer's
verdict:

- All pass: the run completes.
- Any fail or uncertain: status resets to `executing`, the author
  restarts with instructions to read and address the review findings.

**Review runs** (mode=review) invert the entry point. Reviewers run
first with full-codebase scope. If they find issues, the author starts
with the findings. If all reviewers pass, the run completes without
launching the author.

### Resume

`factory resume` finds a run with status `needs-user` or `failed` and
launches an interactive agent session so the user can provide input or
unblock the run.

## Backends

### Local

macOS Seatbelt sandbox. The factory command runs the session loop on the
local machine. Credential injection from Keychain (OAuth token, AWS STS,
Brave Search key). Token refresh at session boundaries.

### Local (bare)

`factory run --no-sandbox` runs the session loop without Seatbelt
sandboxing, worktree creation, or credential refresh. Used on platforms
without macOS sandbox support or when the agent is already isolated by
other means. The agent runs with `--dangerously-skip-permissions` in the
current directory.

### Fargate

Single-container model on AWS ECS Fargate.

```
Local machine                    Fargate task
─────────────                    ────────────
1. create worktree
2. upload worktree → S3
3. start task ────────────►
                                 4. pull workspace from S3
                                 5. session loop (claude -p)
                                 6. ...hours pass...
                                 7. upload workspace → S3
factory status ──────────► (ECS API + S3 check)
factory shell ───────────► (ECS Exec into container)
factory pull ────────────► (download from S3 into worktree)
```

**IAM permissions** (minimal):

| Permission | Scope | Purpose |
|---|---|---|
| `s3:GetObject` | `runs/*` prefix | Pull input workspace |
| `s3:PutObject` | `runs/*` prefix | Upload completed workspace |
| `s3:*` Deny | Outside `runs/*` | Explicit deny on everything else |
| `ssmmessages:*` | `*` | Accept incoming ECS Exec sessions |

Six actions total. No ECS, IAM, STS, or other AWS permissions. The
container can be connected to (ECS Exec) but cannot connect out to other
containers via SSM.

**Infrastructure** (CloudFormation):

- 1 ECR repository (`factory/run`)
- 1 ECS cluster
- 1 task definition (1 vCPU, 2 GB RAM, 30 GB ephemeral storage)
- 1 S3 bucket (30-day lifecycle)
- 1 IAM task role (6 actions)
- 1 IAM execution role (ECR pull + logs)
- 1 security group (egress only)
- CloudWatch log group (optional, infra debugging)

No EFS. Fargate ephemeral storage is sufficient for a single container.

## Credential management

### Local backend

| Credential | Source | Method |
|---|---|---|
| Claude OAuth | macOS Keychain | Extract, pass as env var. Refresh via unsandboxed `claude -p "ok"` at session boundaries. |
| AWS | SSO profile | `aws configure export-credentials` resolves to STS temps, passed as env vars. |
| Brave Search | macOS Keychain | Extract, pass as env var. |

Sandbox profile unchanged — credentials injected via env vars, never by
opening filesystem access.

### Fargate backend

Claude OAuth token passed as env var at task launch. Short-lived; multi-hour
runs will outlive it. Future: WIF (Workload Identity Federation) for
automatic token refresh using the task's IAM identity.

## Repository structure

```
factory/main/
  CLAUDE.md
  documentation/
    architecture.md          ← this file
    behaviors.md             ← behavioral statements (EARS)
  expertise/                 ← factory-level (applies to all projects)
    architecture/
    languages/
  .factory/
    observations.md          ← feedback log (tracked)
    expertise/               ← project-level learnings (tracked)
    runs/                    ← working state (not tracked)
  scripts/
    factory                  ← the factory command
    assets/
      common.sb              ← Seatbelt profile
      claude-code.sb         ← Seatbelt profile
  skills/
    build-in-the-factory/SKILL.md
    capture-brief/SKILL.md
    define-behaviors/SKILL.md
    design-approach/SKILL.md
    plan-execution/SKILL.md
    review-architecture/SKILL.md
    review-behaviors/SKILL.md
    review-documentation/SKILL.md
  infrastructure/
    cloudformation.yaml
    run/
      Dockerfile
      entrypoint.sh
    setup.sh
    teardown.sh
  tests/
    test-skill               ← skill conversation simulation
    test-run                 ← operational behavior assertions
    behaviors/
      skills/                ← scenario cards for test-skill
      README.md              ← behavior-to-test mapping
```

## Skills, expertise, and documentation

Three types of content, each with a different purpose:

**Skills** are procedures — step-by-step instructions an agent follows.
They live in `skills/` and follow the Agent Skills spec.

**Expertise** is reference material for decision-making — principles,
patterns, conventions that inform choices within a procedure. Factory-level
expertise lives in `expertise/` and applies to all projects. Project-level
expertise accumulates in `.factory/expertise/` as patterns are observed
across runs.

**Documentation** describes the system as-built — what it does, how it's
structured, what behaviors are specified. `architecture.md` and
`behaviors.md` describe what IS.

The lifecycle: observations are captured during usage. Some become runs
that build or improve things. Patterns observed across runs accumulate
as project expertise in `.factory/expertise/`.
