---
name: build-in-the-factory
description: Operate the factory workflow to build software autonomously over extended periods. Interactive stages (brief, behaviors, approach, plan) run with the user. Autonomous execution loops writer → tester → parallel reviewers. When all reviewers pass, it produces a Merge Candidate. When a decision needs a human, it sets `needs-user` and pauses, then resumes once the user resolves it.
---

# Build in the Factory

Follow a structured workflow: capture intent, define behaviors, design an approach, plan execution, execute, and review. Some stages need the user; others run autonomously.

Behaviors describe what the system must do; the approach describes how. If execution reveals the approach doesn't work, adapt it — or propose a change via `needs-user` if the change is significant. If a behavior turns out to be wrong or incomplete, pause and renegotiate rather than deliver the wrong thing.

## Work model

The delegated build lifecycle is the Work model: Work Item → Attempt → Task → Workspace → Merge Candidate. Work Items represent planned Factory work, Attempts carry one execution history, Tasks are schedulable units, Workspaces are the filesystem contexts Tasks read or write, and Merge Candidates are reviewed outputs ready to land.

## On session start

Run `factory status` or `factory work list` to see current Work. If stored Work Items exist, inspect the relevant one with `factory work show <work-item-id>`. Continue the latest non-terminal Attempt when the next action is clear, or present the `needs-user` handoff when an Attempt or Merge Candidate asks for user input.

If `factory status` shows a pending Merge Candidate, inspect it with `factory work merge-candidate <work-item-id> <merge-candidate-id>`. Land it with `factory work merge <work-item-id>` after the user accepts the candidate or the policy allows autonomous merging.

If nothing needs attention, ask the user what they want to build.

## Interactive stages (user present)

Follow the four planning skills directly in your session. Each writes into `.factory/drafts/<draft-id>/` — don't create planning files outside that directory:

- `capture-brief` writes `brief.md`.
- `define-behaviors` writes `behaviors.diff.md`.
- `design-approach` writes `approach.md`.
- `plan-execution` writes `plan.md` and creates the Work Item.

For a codebase, module, or area review (not building something new), capture enough context to create a Work Item and use the review-only flow in the autonomous stages below.

## Autonomous stages (user away)

`plan-execution` has already created the Work Item(s) with the approved planning files. From here, use the Work model for delegated execution:

1. Create an Attempt: `factory work attempt <work-item-id>`. (An `attempt-N` id is auto-assigned; pass an explicit id for scripted flows.)
2. Run the Attempt: `factory work attempt run <work-item-id>`. (Defaults to the most recently created Attempt; pass an explicit id to target a specific one.)
3. Inspect status with `factory status` or `factory work show <work-item-id>`.
4. When the Attempt creates a Merge Candidate, inspect it with `factory work merge-candidate <work-item-id> <merge-candidate-id>`.
5. Land through `factory work merge <work-item-id>`. (Defaults to the most recently created Merge Candidate; pass an explicit id to target a specific one.)

Autonomous execution runs as a loop. Each round:

1. The writer produces a candidate commit.
2. The Tester Task runs the project's tests.
3. Domain reviewers evaluate in parallel.

The round outcome determines what happens next:

- All pass — Attempt creates a Merge Candidate.
- Any fail — follow-up write next round, scoped to failed reviewers.
- Any uncertain, or a round cap reached — Attempt records `needs-user`, pausing the loop.

The user provides input; `factory work attempt run` resumes the loop where it left off.

For unrelated work that can proceed in parallel, create independent Work Items.

For codebase, module, or area review-only work, create a Work Item, run `factory work review-codebase <work-item-id> <attempt-id>`, then `factory work attempt run <work-item-id> <attempt-id>`.

## Writer testing contract

The writer produces tests alongside code. When committing a candidate:

- `.factory/tester.yaml` declares the project's test commands (one entry per harness, e.g., Rust nextest + shell).
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

## Factory commands

Use `factory --help` for the top-level surface, `factory work --help` for the Work model commands, and `factory <command> --help` for a specific command's flags. Run `factory cleanup` after terminal Work Items land or fail; `--apply` removes the terminal state.

During interactive stages, follow the skills directly rather than calling these commands ad hoc. `plan-execution` is the one exception — it ends by running `factory work create` as documented in its skill.
