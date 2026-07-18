---
name: fluent
description: Operate the fluent workflow to build software autonomously over extended periods. Interactive stages (brief, behaviors, approach, plan) run with the user. Autonomous execution loops writer → tester → parallel reviewers. When all reviewers pass, it produces a Merge Candidate. When a decision needs a human, it sets `needs-user` and pauses, then resumes once the user resolves it.
---

# Fluent

Follow a structured workflow: capture intent, define behaviors, design an approach, plan execution, execute, and review. Some stages need the user; others run autonomously.

Behaviors describe what the system must do; the approach describes how. If execution reveals the approach doesn't work, adapt it — or propose a change via `needs-user` if the change is significant. If a behavior turns out to be wrong or incomplete, pause and renegotiate rather than deliver the wrong thing.

## Work model

The delegated build lifecycle is the Work model: Work Item → Attempt → Task → Workspace → Merge Candidate. Work Items represent planned Fluent work, Attempts carry one execution history, Tasks are schedulable units, Workspaces are the filesystem contexts Tasks read or write, and Merge Candidates are reviewed outputs ready to land.

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

Run `fluent status` or `fluent work-item list` to see current Work. If stored Work Items exist, inspect the relevant one with `fluent work-item show <work-item-id>`. Continue the latest non-terminal Attempt when the next action is clear, or present the `needs-user` handoff when an Attempt or Merge Candidate asks for user input.

If `fluent status` shows a pending Merge Candidate, inspect it with `fluent merge-candidate show <work-item-id> <merge-candidate-id>`. Land it with `fluent merge-candidate land <work-item-id>` after the user accepts the candidate or the policy allows autonomous merging.

If nothing needs attention, ask the user what they want to build.

## fluent tracks its own state in the repo

fluent git-tracks its durable state in `.fluent/` — its learned project model (`expertise/`) and test config (`tester.yaml`, `extract-tester-results`) — and commits it alongside the user's changes so it persists across runs. On a repo's first run, tell the user this is expected, so they aren't surprised to see `.fluent/` files land in their history.

## Interactive stages (user present)

Follow the four stage procedures in order. Each is a reference file in this skill — read it when entering that stage. Each writes into `.fluent/drafts/<draft-id>/` — don't create planning files outside that directory:

- `references/capture-brief.md` — interview the user and write `brief.md`.
- `references/define-behaviors.md` — elaborate the brief into EARS statements and write `behaviors.diff.md`.
- `references/design-approach.md` — decide the technical approach and write `approach.md`.
- `references/plan-execution.md` — plan the steps and write `plan.md`, then create the Work Item.

For a codebase, module, or area review (not building something new), capture enough context to create a Work Item and use the review-only flow in the autonomous stages below.

## Autonomous stages (user away)

`references/plan-execution.md` has already created the Work Item(s) with the approved planning files. From here, use the Work model for delegated execution:

1. Create an Attempt: `fluent attempt create <work-item-id>`. (An `attempt-N` id is auto-assigned; pass an explicit id for scripted flows.)
2. Run the Attempt: `fluent attempt run <work-item-id>`. (Defaults to the most recently created Attempt; pass an explicit id to target a specific one.)
3. Inspect status with `fluent status` or `fluent work-item show <work-item-id>`.
4. When the Attempt creates a Merge Candidate, inspect it with `fluent merge-candidate show <work-item-id> <merge-candidate-id>`.
5. Land through `fluent merge-candidate land <work-item-id>`. (Defaults to the most recently created Merge Candidate; pass an explicit id to target a specific one.)

Autonomous execution runs as a loop. Each round:

1. The writer produces a candidate commit.
2. The Tester Task runs the project's tests.
3. Domain reviewers evaluate in parallel.

The round outcome determines what happens next:

- All pass — Attempt creates a Merge Candidate.
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
