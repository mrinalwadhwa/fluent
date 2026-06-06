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
│  write-tests                                    │
│  Portable procedures any agent follows          │
├─────────────────────────────────────────────────┤
│  build-in-the-factory skill                     │
│  Teaches agents the full workflow               │
├─────────────────────────────────────────────────┤
│  Factory command                                │
│  factory run / status / pull / shell / watch    │
│  factory resume / init / dashboard / land       │
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

When done, landing a worktree run loads project checks from
`.factory/config.toml`, runs enabled pre-land checks in the worktree,
copies artifacts back from the worktree, removes the worktree, rebases
the run branch onto the source branch, fast-forward merges, deletes the
branch, and sets the status to `landed`. This policy applies to normal
`factory land` runs and to child runs that the parallel orchestrator
lands after each group completes.

Projects opt into checks with this shape:

```toml
[checks.format]
command = "cargo fmt --all -- --check"
fix_command = "cargo fmt --all"
autofix = true
run_before_land = true
```

Factory treats checks generically. If a pre-land check fails without
autofix, land stops before any destructive step. If a check has
`autofix = true` and a `fix_command`, Factory first requires no
uncommitted changes outside `.factory`, runs the fix command, commits
changes outside `.factory` when the fix changes project files, reruns
checks, reruns reviewers after an autofix commit, and continues only
when the required checks and reviews pass.

### Run state

| File | Purpose |
|---|---|
| `brief.md` | User's intent |
| `behaviors.diff.md` | New behaviors this run adds |
| `approach.md` | Solution direction and expertise references |
| `plan.md` | Execution steps |
| `status` | `briefed`, `behaviors-defined`, `approach-designed`, `planned`, `executing`, `reviewing`, `rate-limited`, `needs-user`, `complete`, `failed`, `landed` |
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
| `reviews/` | Review artifacts, transcripts (`transcript-{name}.jsonl`), and round archives (`round-N/`) |
| `children` | Child run IDs, one per line (written by the parallel orchestrator for parent runs) |
| `parent` | Parent run ID (written for each child run) |

### Run-id resolution

The factory command resolves the run-id through a priority chain:

1. `--run-id` flag
2. `FACTORY_RUN_ID` environment variable
3. `.factory/active-run` pointer file
4. Scan `.factory/runs/` for active status (fallback)

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
    run pre-land checks and merge each child's branch into parent branch
    set each child's status to landed
record children list in parent run dir
set parent status to complete
```

The parent run's session loop never executes — the orchestrator
(`parallel::run_parallel_plan`) replaces it entirely. Each child run
gets its own session loop in its own worktree.

After the orchestrator completes, all children are already merged and
landed. `factory land` on the parent run verifies all children are
landed and sets the parent status to `landed` — there is no worktree
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
revert, or intentionally ignore the remaining work. Review-only runs
complete after reviewers finish because they do not launch an author to
modify the worktree. The landing path also rejects dirty completed
worktrees before removing them, so uncommitted author output is not
discarded during land.

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
when the caller has not already set it, using a file-based CA bundle so
Codex can connect without Keychain IPC. `FACTORY_CODEX_CA_BUNDLE`
overrides the detected bundle path. In bare mode, Codex also runs with
`--dangerously-bypass-approvals-and-sandbox`, but without
`sandbox-exec`.

Fargate currently supports only Claude because its container entrypoint
and credential path remain Claude-specific.

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
`.factory/runs/[run-id]/reviews/`. The loop parses each reviewer's
verdict:

- All pass: the run completes only if the worktree is clean; if
  uncommitted changes remain outside `.factory`, the loop writes a
  handoff, sets status back to `executing`, and restarts the author to
  resolve them.
- Any fail or uncertain: status resets to `executing`, the author
  restarts with instructions to read and address the review findings.

If the run exceeds the review-round limit, the loop accepts the current
review state with the same clean-worktree guard: clean work completes,
while uncommitted work receives a handoff and returns to `executing`.

Review runs (mode=review) produce findings only. Reviewers run with
full-codebase scope. Their findings are written to the reviews/
directory and the run completes. No author session is launched.

### Resume

`factory resume` finds a run with status `needs-user` or `failed` and
launches an interactive agent session with the selected coder so the
user can provide input or unblock the run.

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

#### IAM permissions (minimal)

| Permission | Scope | Purpose |
|---|---|---|
| `s3:GetObject` | `runs/*` prefix | Pull input workspace |
| `s3:PutObject` | `runs/*` prefix | Upload completed workspace |
| `s3:*` Deny | Outside `runs/*` | Explicit deny on everything else |
| `ssmmessages:*` | `*` | Accept incoming ECS Exec sessions |

Six actions total. No ECS, IAM, STS, or other AWS permissions. The
container can be connected to (ECS Exec) but cannot connect out to other
containers via SSM.

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
  Cargo.toml                 ← Rust crate definition
  Cargo.lock
  src/
    main.rs                  ← CLI dispatch (clap)
    lib.rs                   ← public API for tests
    coder.rs                 ← Coder trait + Claude/Codex implementations
    cli.rs                   ← CLI argument types
    content.rs               ← Content resolution (project → user → bundled)
    credential.rs            ← Keychain credential injection
    run.rs                   ← Run state, resolution, status
    session.rs               ← Session loop orchestration
    review.rs                ← Review loop, verdict parsing
    os.rs                    ← Seatbelt sandbox rendering, prerequisites
    worktree.rs              ← Git worktree operations
    report.rs                ← Report generation
    fargate.rs               ← Fargate launch, pull, shell
    dashboard.rs             ← Live TUI for run activity
    transcript.rs            ← Parse stream-json transcripts incrementally
    plan.rs                  ← Parse plan.md into groups and steps
    parallel.rs              ← Parallel plan orchestrator (child runs)
  documentation/
    architecture.md          ← this file
    behaviors.md             ← behavioral statements (EARS)
  expertise/                 ← factory-level (applies to all projects)
    architecture.md
    documentation.md
    shell-scripts.md
    skills.md
    tests.md
  .factory/
    observations.md          ← feedback log (tracked)
    expertise/               ← project-level learnings (tracked)
    runs/                    ← working state (not tracked)
  prompts/                   ← agent system prompts
    author.md
    review-architecture.md
    review-behaviors.md
    review-documentation.md
    review-skills.md
    review-tests.md
  scripts/
    factory                  ← shell script (legacy, used by Fargate entrypoint)
    assets/
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
    write-documentation/SKILL.md
    write-documentation/references/
    write-tests/SKILL.md
    write-tests/references/
  infrastructure/
    cloudformation.yaml
    run/
      Dockerfile
      entrypoint.sh
    setup.sh
    teardown.sh
  tests/
    behaviors/
      operations/            ← behavioral tests for the Rust binary
      skills/                ← scenario cards for test-skill
      README.md              ← behavior-to-test mapping
```

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

The factory resolves prompts, sandbox profiles, skills, and expertise
files through a three-tier search chain. First match wins, no merging:

1. **Project-local**: `<project>/.factory/<relative_path>`
2. **User config**: `~/.config/factory/<relative_path>`
3. **Bundled defaults**: compiled into the binary at build time

This lets projects override any default content without modifying the
binary, and lets users set personal defaults across projects.
