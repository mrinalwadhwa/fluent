# Architecture

Workflow and execution system for autonomous coding agents. Manages work
from intent capture through execution and review across multiple sessions.

## System overview

```
┌─────────────────────────────────────────────────┐
│  Skills                                         │
│  capture-brief, define-behaviors,               │
│  design-approach, plan-execution                │
│  Portable procedures any agent follows          │
├─────────────────────────────────────────────────┤
│  build-in-the-factory skill                     │
│  Teaches agents the full workflow               │
├─────────────────────────────────────────────────┤
│  Factory command                                │
│  factory run / status / pull / shell / watch    │
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
  session-2/
    ...
```

First-class learning artifacts, not debug logs.

## Agents

### Author

Implements code. Follows the plan. Pauses when genuinely uncertain rather
than drifting.

### Reviewers

Evaluate the author's output. Two categories:

**White-box** (see code): code reviewer, architecture reviewer, security
reviewer.

**Black-box** (observe behavior only): behavior reviewer, pen test
reviewer, UX reviewer. Cannot see code — cannot rationalize away problems
by reading the implementation.

Review verdicts: **pass** / **uncertain** (ask user) / **fail** (send back).

## Backends

### Local

macOS Seatbelt sandbox. The factory command runs the session loop on the
local machine. Credential injection from Keychain (OAuth token, AWS STS,
Brave Search key). Token refresh at session boundaries.

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
