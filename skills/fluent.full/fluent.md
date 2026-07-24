---
name: fluent
description: Operate the fluent workflow to build software autonomously over extended periods. Interactive stages (brief, behaviors, approach, plan) run with the user. Autonomous execution loops writer → tester → parallel reviewers, then runs the Learner after a passing review round. Only a successful Learner run produces a ready Merge Candidate. A retryable Learner failure resumes with `fluent attempt run`; a non-relaunchable evidence failure stays blocked for human recovery. When a decision needs a human, it sets `needs-user` and pauses, then resumes once the user resolves it.
---

# Fluent

Follow a structured workflow: capture intent, define behaviors, design an approach, plan execution, execute, and review. Some stages need the user; others run autonomously.

Behaviors describe what the system must do; the approach describes how. If execution reveals the approach doesn't work, adapt it — or propose a change via `needs-user` if the change is significant. If a behavior turns out to be wrong or incomplete, pause and renegotiate rather than deliver the wrong thing.

## Work model

The delegated build lifecycle is the Work model: Work Item → Attempt → Task → Workspace → Merge Candidate. Work Items represent planned Fluent work, Attempts carry one execution history, Tasks are schedulable units, and Workspaces are the filesystem contexts Tasks read or write. A Merge Candidate record may exist while the Learner is retryable; it becomes ready to land only after the Learner succeeds.

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

1. The writer produces a candidate commit.
2. The Tester Task runs the project's tests.
3. Domain reviewers evaluate in parallel.
4. After the reviewers pass, the Learner captures durable project expertise
   and records possible follow-ups for materialization after land.

The round outcome determines what happens next:

- Reviewers pass and the Learner succeeds — Attempt creates a ready Merge Candidate.
- Learner fails with a relaunchable disposition — the Merge Candidate remains
  non-ready and cannot land; `fluent attempt run` retries only the Learner.
- Learner fails after its coder ran but host evidence remains pending — the
  candidate is `learner-blocked`; inspect the Work Item and recover the evidence
  with human intervention. Do not rerun the Learner or land the candidate.
- Any fail — follow-up write next round, scoped to failed reviewers.
- Any uncertain, or a round cap reached — Attempt records `needs-user`, pausing the loop.

The user provides input; `fluent attempt run` resumes the loop where it left off.

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

2. After the user chooses, run `fluent init`.

3. If the user chose `propose`, leave `.fluent/config.yaml` unchanged.

4. If the user chose `execute`, write this nested mapping to
   `.fluent/config.yaml` after init:

   ```yaml
   follow-up:
     mode: execute
   ```

`execute` authorizes and queues trusted corrective Work. It does not start
execution. A human must separately run `fluent scheduler run`; any resulting
ready Merge Candidate still requires human inspection and landing.

## Writer testing contract

The writer produces tests alongside code. When committing a candidate:

- `.fluent/tester.yaml` declares the project's test commands (one entry per harness, e.g., Rust nextest + shell).
- Each EARS statement has either a `Test:` reference pointing at a real test or an `Untestable:` marker with a one-line reason.
- Run the project's tests before committing (best practice, not enforced).

The Tester Task runs after the write completes and produces `tester-results.json` for reviewers.

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
