---
name: fluent
description: Operate Fluent, a self-improving software factory. Use when a user wants to review, build, fix, or improve software with Fluent. Invoke when they ask to install or initialize Fluent; capture an Observation; define a slice; create or refine a Brief, Behavior Specification, Technical Approach, Implementation Plan, or Work Item; run, queue, inspect, resume, or recover an Attempt; review a codebase through Fluent; manage or land a Merge Candidate; capture project Expertise; or configure Fluent's agents, scheduler, sandboxes, or remote execution.
---

# Fluent

Follow a structured workflow: capture intent, define behaviors, design an approach, plan execution, execute, and review. Some stages need the user; others run autonomously.

Behaviors describe what the system must do; the approach describes how. If execution reveals the approach doesn't work, adapt it — or propose a change via `needs-user` if the change is significant. If a behavior turns out to be wrong or incomplete, pause and renegotiate rather than deliver the wrong thing.

## Work model

The delegated build lifecycle is the Work model: Work Item → Attempt → Task → Workspace → Merge Candidate. Work Items represent planned Fluent work, Attempts carry one execution history, Tasks are schedulable units, and Workspaces are the filesystem contexts Tasks read or write. A Merge Candidate record may exist while the Learner is retryable; it becomes ready to land only after the Learner succeeds.

Only a successful Learner run produces a ready Merge Candidate.

## Make sure fluent is installed

Everything below uses the `fluent` command. Check that it is available before running any other step:

```sh
fluent --version
```

If `fluent` is not found, install it and check again:

```sh
curl -fsSL fluent.computer/install | sh
fluent --version
```

The installer puts `fluent` in `~/.local/bin`. If the second check still fails, that directory is not on the current `PATH`: run the rest of this workflow with the full path `~/.local/bin/fluent`, and tell the user to add `~/.local/bin` to their `PATH` for future sessions.

## On session start

First check whether `.fluent/` exists. If it does not, complete
“First-time project setup” below before running `fluent status` or any Work
command.

Run `fluent status` or `fluent work-item list` to see current Work. If stored Work Items exist, inspect the relevant one with `fluent work-item show <work-item-id>`. Continue the latest non-terminal Attempt when the next action is clear, or present the `needs-user` handoff when an Attempt or Merge Candidate asks for user input.

If status, list, or show warns that a Work Item has an unknown pause kind or was
written by a newer Fluent version, keep the Work Item read-only. Upgrade Fluent
before running a Task, Attempt, landing, cleanup, or any command that would
rewrite its model; compatibility gates intentionally reject those mutations.

If `fluent status` shows a `merge-ready` Merge Candidate, inspect it with
`fluent merge-candidate show <work-item-id> <merge-candidate-id>`. Present it
to the user for inspection. Run `fluent merge-candidate land <work-item-id>`
only after the user accepts the candidate. Do not start `fluent auto-merge`;
it is outside the Local Preview.

If nothing needs attention, ask the user what they want to build.

## Fluent tracks its own state in the repo

fluent stores its learned project model (`expertise/`) and test config (`tester.yaml`, `extract-tester-results`) in `.fluent/` and commits them alongside the user's changes so they persist across runs. On a repo's first run, tell the user this is expected, so they aren't surprised to see `.fluent/` files in their history.

## Interactive stages (user present)

Follow the four stage procedures in order. Each is a reference file in this skill — read it when entering that stage. Each writes into `.fluent/drafts/<draft-id>/` — don't create planning files outside that directory:

- `references/capture-brief.md` — interview the user and write `brief.md`.
- `references/define-behaviors.md` — elaborate the brief into EARS statements and write `behaviors.diff.md`.
- `references/design-approach.md` — decide the technical approach and write `approach.md`.
- `references/plan-execution.md` — plan the steps and write `plan.md`, then create the Work Item.

For a codebase, module, or area review (not building something new), capture enough context to create a Work Item and use the review-only flow in the delegated stages below.

## Delegated execution

`references/plan-execution.md` has already created the Work Item(s) with the approved planning files. From here, use the Work model for delegated execution:

1. Create an Attempt: `fluent attempt create <work-item-id>`. (An `attempt-N` id is auto-assigned; pass an explicit id for scripted flows.)
2. Run the Attempt: `fluent attempt run <work-item-id>`. (Defaults to the most recently created Attempt; pass an explicit id to target a specific one.)
3. Inspect status with `fluent status` or `fluent work-item show <work-item-id>`.
4. Stop when the Attempt produces a ready Merge Candidate. Present it to the
   user for inspection with
   `fluent merge-candidate show <work-item-id> <merge-candidate-id>`.
5. Only after the user explicitly accepts that candidate, run
   `fluent merge-candidate land <work-item-id>`. (Defaults to the most recently
   created Merge Candidate; pass an explicit id to target a specific one.)

Delegated execution runs as a loop until it produces a ready Merge Candidate,
stops at a Learner failure, or pauses at `needs-user`. Each round:

1. The Writer produces a candidate commit, updates required progress, and fills
   the host-generated `writer-completion.json` coverage matrix with implementation
   evidence and focused harness-native verification.
2. Fluent reconciles required progress and the completion matrix. If approved
   work remains incomplete, it
   resumes the same Writer provider session without running the Tester or
   reviewers. The continuation reads a bounded generated context containing the
   exact candidate/base commits, unresolved progress, changed files, current
   deduplicated findings, passing evidence, executed commands, and paths to full
   historical artifacts. If the session identity is unavailable, Fluent starts a
   fresh persistent Writer session.
3. Once required progress and every matrix row are complete, domain reviewers
   evaluate the candidate in parallel using the matrix and Writer's focused
   verification as advisory evidence.
4. After the reviewers pass, one final Tester Task runs the project's complete
   declared test commands against the exact reviewed commit.
5. After the final Tester passes, the Learner runs in the Work Item's Learner mode:
   in the default `capture` mode it captures durable project expertise, while in
   `no-expertise` mode it audits the change without writing expertise. Either
   way it records possible follow-ups for materialization after land.

The round outcome determines what happens next:

- Reviewers pass and the Learner succeeds — Attempt creates a ready Merge Candidate.
- Learner fails with a relaunchable disposition — the Merge Candidate remains
  non-ready and cannot land; `fluent attempt run` retries only the Learner.
- Learner fails after its coder ran but host evidence remains pending — the
  candidate is `learner-blocked`; inspect the Work Item and recover the evidence
  with human intervention. Do not rerun the Learner or land the candidate.
- Any fail — follow-up write next round, scoped to failed reviewers, except
  when a failed reviewer needs trusted host evidence for an unchanged candidate.
- Any uncertain verdict — Attempt records `needs-user`, pausing the loop.
- A Writer-round cap reached — inspect the candidate and failed reviews. If the
  frontier is legitimate, approve one to three more rounds with `fluent attempt
  extend <work-item-id> <attempt-id> --additional-write-rounds N`, then resume.
  Fluent binds the approval to the candidate and exact review bytes.
- An incomplete Writer without a new candidate commit, regressed progress or
  matrix completion, or two consecutive pre-review continuations with no newly
  completed requirement — Attempt records `needs-user` before Tester and review.
- A missing, malformed, contradictory, or structurally edited completion matrix —
  Attempt records `needs-user`; restore the host-owned rows before resuming.
- A provider exhausts its retries before the model produces tokens or uses tools — Attempt records `needs-user` as `provider-unavailable`.

The user provides input; `fluent attempt run` resumes the loop where it left off.
When the pause is `provider-unavailable`, wait for provider capacity, then run
`fluent attempt run <work-item-id> [attempt-id]` without new input. For a hung or
interrupted local coder, run `fluent attempt cancel <work-item-id> <attempt-id>`;
after Fluent confirms its owned process group stopped, the same `attempt run`
command replans only the canceled Task. Fluent keeps completed peer Tasks and
prior transcripts.

### Recover a host-evidence finding

Use this only when the candidate does not need a source change and the failed
review explicitly asks for proof that a trusted host can run. Create one JSON
file on that host, for example:

```json
{"schema_version":1,"producer":"release-host","check":"fluent tester check","working_directory":"/repo","result":"pass","run_at":"2026-08-03T17:59:47Z","output":"...captured output..."}
```

Inspect `fluent work-item show <work-item-id>` for the current Attempt,
candidate SHA, and failed `review.md` artifact paths. Then attach the exact
proof and each finding it addresses:

```sh
fluent attempt evidence attach <work-item-id> <attempt-id> \
  --candidate <candidate-sha> --evidence-file host-evidence.json \
  --review-artifact .fluent/work/artifacts/.../review.md
```

Fluent snapshots the exact document, binds it to that candidate, and reruns
only the named reviewers. Do not edit the candidate or invent a commit. Inspect
the Work Item again for the next action: `evidence-needed` means attach new
host proof; `code-change` means run the ordinary Writer path. A passing targeted
review still requires all exact-commit evidence and Learning before landing.

### Codex authentication pauses

Autonomous Codex workers use a private authentication home and do not load the
interactive Codex configuration or hooks. If Fluent pauses an Attempt for Codex
authentication, run `codex login`, then resume the same work with `fluent attempt
run <work-item-id> [attempt-id]`. Interactive Codex sessions continue to use the
user's normal configuration and hooks.

Autonomous Claude Writers use an Attempt-scoped managed home. Their sandbox
blocks the operator's persistent Claude project and memory tree; do not copy
memory or session state from `~/.claude/projects` into a Work Item manually.

For unrelated work that can proceed in parallel, create independent Work Items.

For codebase, module, or area review-only work, create a Work Item, run `fluent review codebase <work-item-id> <attempt-id>`, then `fluent attempt run <work-item-id> <attempt-id>`.

### Coder selection

`fluent attempt run` prints the resolved coder, model, and effort for each role
(writer, reviewer, behavior-tests) before the first round. Before launching an
expensive run, present this plan to the user and confirm. Override per-run with
`--coder`, `--model`, `--effort`, or per-role variants (`--write-model`,
`--review-effort`, etc.). Configure defaults in `.fluent/config.yaml` (project)
or `~/.config/fluent/config.yaml` (user):

```yaml
coders:
  writer:
    coder: claude
    # model: optional — omit to use the coder's own default
    effort: high
  reviewer:
    coder: claude
  behavior-tests:
    coder: claude
```

## Local Preview

Fluent's first release is the **Local Preview**: a supervised, local-first path you can try before its background execution, interruption, concurrency, and remote-execution hardening is complete. The default path stays visible and human-controlled:

- Attempts run **locally in the foreground** — you watch each round as it happens.
- Corrective follow-up findings become **proposed follow-up Work** by default.
- Release exercises start with stored acceptance criteria. New findings remain
  proposed follow-ups unless they map directly to one of those criteria; use
  `fluent work-item classify-finding` to record the decision.
- `fluent work-item authorize <work-item-id>` authorizes and enqueues proposed
  Work. Authorization does not run an Attempt and never authorizes landing.
- Queued Work starts only while a human explicitly runs `fluent scheduler run`.
  The scheduler never lands a candidate; after successful Learning it stops at
  a ready Merge Candidate.
- **Every ready Merge Candidate is inspected and landed by a human** with
  `fluent merge-candidate land <work-item-id>`.
- Post-merge review is **off by default** and remains a positive per-land
  opt-in with `fluent merge-candidate land --post-merge-review`.

`fluent auto-merge`, automatic scheduler lifecycle, automatic landing, and
Fargate are outside the Local Preview.

## First-time project setup

When `.fluent/` does not exist:

1. Before running `fluent init`, ask:

   ```text
   Which follow-up mode should this project use?

   (a) propose — corrective findings become proposed Work you authorize
       (recommended: keeps the Local Preview human-controlled)
   (b) execute — corrective findings are authorized and queued automatically
   ```

2. After the user chooses, ask:

   ```text
   Which coder profile should Fluent save for this project?

   (a) codex-balanced — Codex, gpt-5.6-terra, medium effort for the writer,
       reviewers, and behavior-test coder
       (recommended: balanced capability and speed)
   (b) codex-stronger — Codex, gpt-5.6-sol, medium effort for the writer,
       reviewers, and behavior-test coder
       (stronger reasoning when the extra time is worthwhile)
   (c) custom — choose a coder, model, and effort for each role
   ```

3. If the user chooses `custom`, explain before collecting values: the writer
   implements the change, reviewers evaluate it from independent perspectives,
   and the behavior-test coder writes or updates tests. Ask separately for the
   coder, model, and effort for each role.

4. After the user completes both choices, run one configured command. For a
   curated choice, use the selected profile identifier:

   ```sh
   fluent init --coder-profile codex-balanced --follow-up-mode propose
   ```

   For `custom`, use `--coder-profile custom` and pass every `--write-*`,
   `--review-*`, and `--behavior-tests-*` value the user chose.

5. The command preflights each distinct provider before it initializes the
   project. If it reports a failure, name the failed provider and condition,
   then offer the user a retry or a different profile. A successful check only
   verifies the local command and authentication readiness; it does not promise
   future provider capacity.

6. After success, show the saved coder, model, and effort for the writer,
   reviewers, and behavior-test coder. Explain that every new Attempt stores
   the effective mapping unless the user supplies explicit Attempt overrides.

`execute` authorizes and queues trusted corrective Work. It does not start
execution. A human must separately run `fluent scheduler run`; any resulting
ready Merge Candidate still requires human inspection and landing.

## Writer testing contract

The writer produces tests alongside code. When committing a candidate:

- `.fluent/tester.yaml` declares the project's test commands (one entry per harness, e.g., Rust nextest + shell).
- Each EARS statement has either a `Test:` reference pointing at a real test or an `Untestable:` marker with a one-line reason.
- Run focused harness-native selections before committing and report the exact
  commands, results, and covered risks. Leave the complete configured suite to
  Fluent's final Tester.

Reviewer Tasks run only after a completed Writer has satisfied the Work Item's
required-progress contract and task-specific `writer-completion.json`. Fluent
generates immutable rows from the approved behaviors and their existing
`Test:`/`Untestable:` markers, applicable Approach decisions, structural
boundaries, and execution guidance, required Plan rows and Verification cells,
and the current round's open review findings. This references harness-native
commands; it does not introduce
another selector vocabulary. The Writer fills concrete implementation evidence,
passing focused commands, and each finding's applicable behavior or approach
constraint (or a specific reason none applies). Incomplete work continues on the
Writer path and does not spend a review or Tester cycle. Domain reviewers inspect
the candidate and use the matrix and reported focused checks as advisory evidence
instead of rerunning the full suite. Only the tests reviewer may run one named
missing check, using the candidate-keyed shared cache.

After every reviewer passes, one final Tester produces `tester-results.json`,
which the host binds to the exact reviewed commit. This is the authoritative
complete-suite gate before Learning. A failed review therefore returns to the
Writer without spending a full Tester run; a final Tester regression returns its
artifact to a corrective Writer.

`fluent status` and `fluent work-item show <work-item-id>` also report cycle-cost
measurements: review rounds, completed-stage duration, local transcript tokens,
repeated findings, artifact bytes, and pre-review cycles avoided. Use these to
spot a Work Item whose context or review loop is growing before starting another
expensive round. These commands read one persisted sidecar instead of walking
artifact trees. After upgrading an existing project or manually repairing
artifacts, run `fluent work-item rebuild-metrics [work-item-id]` once.

After creating or repairing a project Tester, run `fluent tester check` before spending review work. It validates the Tester and runs it through the production Tester boundary. If this standalone check reports a harness error, repair the configuration, extractor, or sandbox problem and rerun `fluent tester check`; it creates no Attempt to resume. If a production Tester Task pauses an existing Attempt for a harness error, repair the problem and resume with `fluent attempt run <work-item-id> [<attempt-id>]`; the same Tester retries without rerunning an already completed Writer. For SwiftPM nested-sandbox failures, disable SwiftPM's inner sandbox and use writable project-local cache paths. Fluent leaves project test configuration and scripts unchanged.

## When to pause

Pause and set status to `needs-user` when:
- You are genuinely uncertain about intent, approach, or scope
- You discover a defined behavior is wrong or incomplete
- You need to deviate significantly from the approach
- A reviewer returns `uncertain`
- You encounter a decision with significant consequences that could go multiple ways
- You need access, credentials, or information you don't have

Don't pause for:
- Decisions you can make confidently from context
- Minor implementation choices within the approach
- Things you can verify by reading the code or running tests

## Fluent commands

Use `fluent --help` for the top-level surface and `fluent <command> --help` for a specific command's flags. Run `fluent cleanup` after terminal Work Items land or fail; `--apply` removes the terminal state.

During interactive stages, follow the stage references directly rather than calling these commands ad hoc. `references/plan-execution.md` is the one exception — it ends by running `fluent work-item create` as documented in its procedure.
