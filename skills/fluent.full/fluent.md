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

Offer this choice once, early in planning after the request is grounded enough
to explain it accurately:

> "Planning normally works one decision at a time. At any point, you can say
> **use your judgment through the rest of planning**. I’ll choose my
> recommendations through the brief, behaviors, approach, and plan, then ask for
> one confirmation after the last applicable planning artifact. I’ll still stop
> for behavior or scope changes, consequential tradeoffs, and missing
> information."

Do not repeat the offer after the user has chosen a collaboration style.

## Planning-wide delegation

Planning-wide delegation starts only when the user explicitly asks Fluent to use
its judgment or recommendations through the rest of planning. "Keep going" does
not activate delegation. The user may activate it at any point, revoke it at any
point, or narrow it in their own words. Do not infer it from prior sessions,
saved drafts, silence, or a request to work autonomously during execution.

If the user revokes delegation before the final confirmation, return to the
earliest provisional artifact, present it through its normal confirmation gate,
and resume the ordinary stage-by-stage path. Never promote provisional artifacts
to approved merely because delegation ended.

While it is active:

- Make recommendation-level, reversible planning choices from the user’s stated
  goal, repository evidence, recorded project decisions, and the applicable
  stage guidance. Record chosen assumptions, alternatives, and tradeoffs in the
  provisional artifacts.
- Draft the remaining brief, behaviors, approach, and plan without
  question-by-question decisions or intermediate artifact confirmations. These
  files remain provisional and must not be called approved.
- Give concise progress updates when useful, but do not turn them into implied
  approval gates.
- After the last applicable planning artifact, show the complete planning set,
  the consequential choices Fluent made, every proposed Work Item and Learner
  mode, and any release criteria. Ask for one final confirmation. If the user
  requests a revision, update every affected downstream artifact and present the
  complete set again.

A mandatory interruption overrides delegation. Stop and ask one focused question
when resolving it would change a user-stated or already accepted behavior or
scope; add an outcome that is not grounded in the request or project; make a
consequential or difficult-to-reverse tradeoff; contradict a recorded project
decision; require information or access Fluent cannot obtain; authorize large
scope or another exceptional policy; or decide something outside planning.
Research facts that can be discovered from the project or authoritative sources
without asking the user. Once the user resolves a true interruption, delegation
remains active unless they revoke it.

Delegation expires after the final planning confirmation. Delegation does not
authorize Work Item creation, Attempt creation, execution, or landing. The final
planning confirmation may authorize the documented Work Item creation step, but
Attempt execution and landing retain their existing separate boundaries.

For a codebase, module, or area review (not building something new), capture enough context to create a Work Item and use the review-only flow in the delegated stages below.

## Recover an enclosing agent sandbox

Configured initialization and local execution may report `FLUENT_HOST_SANDBOX_BLOCKED`
when an enclosing Codex or Claude Code sandbox prevents Fluent from applying its
worker sandbox. Rerun the same command through the host agent's scoped
unsandboxed approval. In Codex, request escalated execution for that command.
In Claude Code, retry that Bash command with its unsandboxed-command escape hatch.
Tell the user the approval applies only to Fluent; Fluent will still sandbox every
worker. Do not add Fluent's `--no-sandbox`; do not switch the whole host-agent
session to full access. This is an enclosing host-agent boundary, not a provider
login failure; do not treat it as a reason to run `codex login` or `claude /login`.
Continue with the same command and Work Item or Attempt.

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

1. The Writer produces a candidate commit, updates mutable progress sections, and
   fills Writer-owned rows and applicable findings in the host-generated
   `writer-completion.json` matrix with implementation evidence and focused
   harness-native verification. External rows remain pending with empty evidence;
   Fluent projects Writer completion into the required progress section.
   Every Writer round in the Attempt shares a confined Cargo cache, seeded
   only with the host's admitted `registry/` and `git/` stores; Writers do not
   use or modify the operator's global Cargo home.
2. Fluent reconciles Writer-owned progress, matrix rows, and applicable findings.
   If that work remains incomplete and the no-output, regression, stagnation,
   and structural pause rules permit continuation, it resumes the same Writer
   provider session without running the Tester or reviewers. When finished
   Writer work leaves a required host check next, Fluent pauses with the exact
   candidate, command, and required evidence, including after earlier reviews.
   Passing host evidence must complete targeted Tests Review; host and later
   workflow obligations remain required outside Writer completion.
   The continuation reads a bounded generated context containing the
   exact candidate/base commits, unresolved progress, changed files, current
   deduplicated findings, passing evidence, executed commands, and paths to full
   historical artifacts. If the session identity is unavailable, Fluent starts a
   fresh persistent Writer session.
3. Before admission to review, Fluent runs cheap deterministic checks against
   the exact Writer delta: Git whitespace and commit integrity, approved
   behavior-to-test references, and the project's read-only `check-pre-merge`
   hook. A first deterministic defect returns its structured evidence to one
   corrective Writer; a repeated defect or unverifiable host state pauses. A
   reviewed correction resumes the compatible Writer session, reads one bounded
   current-context artifact, and references full historical evidence by path.
   It saves complete command output under its Task artifact and keeps only
   summaries or relevant failure excerpts in the model conversation.
4. Once Writer-owned progress and matrix rows, applicable findings, required host
   evidence with targeted Tests Review, and the deterministic gate are satisfied,
   domain reviewers evaluate the candidate in parallel using the matrix, gate
   artifact, and Writer's focused verification as advisory evidence. All ordinary
   domains must pass; targeted evidence review supplies no ordinary coverage.
   Corrections rerun failed, changed, or missing roles. Carry-forward requires
   known unchanged domains across every intervening Writer delta and valid context.
5. After the reviewers pass, one final Tester Task runs the project's complete
   declared test commands against the exact reviewed commit.
6. After the final Tester passes, the Learner runs in the Work Item's Learner mode:
   in the default `capture` mode it captures durable project expertise, while in
   `no-expertise` mode it audits the change without writing expertise. Either
   way it records possible follow-ups for materialization after land. Capture
   changes are restricted to Fluent's expertise subtree and cross the final
   deterministic candidate gate before the handoff is published; they do not
   invalidate the pre-Learner domain reviews.

The round outcome determines what happens next:

- Reviewers pass and the Learner succeeds — Attempt creates a ready Merge Candidate.
- Learner fails with a relaunchable disposition — the Merge Candidate remains
  non-ready and cannot land; `fluent attempt run` retries only the Learner.
- Learner fails after its coder ran but host evidence remains pending — the
  candidate is `learner-blocked`; inspect the Work Item and recover the evidence
  with human intervention. Do not rerun the Learner or land the candidate.
- Any fail — follow-up write next round, then rerun the failed reviewers and any
  additional review domains the correction changed. Passing untouched domains
  remain valid. A reviewer that needs trusted host evidence for an unchanged
  candidate follows the separate host-evidence recovery path.
- Any uncertain verdict — Attempt records `needs-user`, pausing the loop.
- A Writer-round cap reached — inspect the candidate and failed reviews. If the
  frontier is legitimate, approve one to three more rounds with `fluent attempt
  extend <work-item-id> <attempt-id> --additional-write-rounds N --resolution
  '<authorization>'`, then resume.
  Fluent binds the approval to the candidate and exact review bytes. A scoped
  Writer produced by an evidence-targeted `code-change` review already counts
  in the effective cap, so the extension starts from the Writer frontier shown
  in the handoff.
- An incomplete Writer without a new commit, regressed completion, or two stagnant continuations against unchanged obligations, including after review, pauses at `needs-user` before Tester and review.
  For a committed Writer with a valid incomplete matrix and proven stagnation, follow [Recover a stalled committed Writer](references/host-evidence-recovery.md#recover-a-stalled-committed-writer).
- A repeated deterministic candidate rejection, unreadable gate evidence, or a
  project check that changes HEAD or the worktree — Attempt records `needs-user`
  before Tester and review. Inspect `candidate-gate.json` and repair the named
  candidate or project check before resuming.
- A missing, malformed, contradictory, or structurally edited completion matrix records `needs-user`. For a committed Writer's malformed matrix, follow [Recover a committed Writer with a malformed matrix](references/host-evidence-recovery.md#recover-a-committed-writer-with-a-malformed-matrix).
  Preserve the original bytes and completed Task. Diagnose other matrix failures separately; do not edit host-owned rows to force completion.
- A provider exhausts its retries before the model produces tokens or uses tools — Attempt records `needs-user` as `provider-unavailable`.

When an uncertain review came from an external problem and the candidate does
not need to change, repair that problem and run `fluent attempt run`. Fluent
first verifies the unchanged clean candidate and the latest review evidence,
then reruns only the uncertain reviewer roles while preserving passing reviews.
For scheduler-owned Work, use the exact `fluent queue resume <work-item-id>
--attempt <attempt-id>` command reported by status; queue admission applies the
same checks before binding the existing Attempt.
When a pre-land Learner fails with a relaunchable disposition, the scheduler
records the dispatch as blocked. Run `fluent queue resume <work-item-id>
--attempt <attempt-id>`, then `fluent scheduler run`. Fluent verifies that the
reviewed candidate is still clean and unchanged before queueing and again at
claim, then retries only the Learner. Use `fluent attempt run <work-item-id>
<attempt-id>` for the equivalent foreground recovery.
When the pause is `provider-unavailable`, read the needs-user handoff first.
If it identifies invalid pass-through provider arguments, remove or correct those
arguments, then run `fluent attempt run <work-item-id> [attempt-id]` to retry the
same Task. If it identifies exhausted provider capacity, wait for capacity, then
run that same command without new input. To stop a local Attempt, run
`fluent attempt cancel <work-item-id> <attempt-id>`. Fluent persists the stop
before settling owned Writer, verification, Tester, review, and Learner workers.
No downstream stage starts until explicit resume. Repeating cancellation after
settlement returns the settled result without changing history. Run
`fluent attempt run <work-item-id> <attempt-id>` after settlement to retry
unfinished work. Fluent preserves completed Tasks and prior artifacts; canceled
verification retries use a new artifact directory.

When an uncertain Architecture review has a ratified replacement design, save
the complete replacement Approach and run:

```text
fluent attempt revise-approach <work-item-id> <attempt-id> --approach-file <path> --resolution '<decision>'
```

Then resume with `fluent queue resume <work-item-id> --attempt <attempt-id>` or
foreground `fluent attempt run <work-item-id> <attempt-id>`.

For a failed-review `round-cap` pause, read [round-cap Approach recovery](references/round-cap-approach-recovery.md) before acting; it covers extension, revision, explicit resume, exact retry, and later gates.

### Recover a host-evidence finding

Read [host-evidence recovery](references/host-evidence-recovery.md) for
host-only evidence attachment, candidate-bound source correction, and legacy rules.
Use `fluent attempt evidence run` to schedule an approved check, `attach` for an
existing schema-version-1 result, and `classify` when failed evidence authorizes
source correction. Follow the printed `fluent task run` and `fluent attempt run`
commands to execute the check, validate its receipt, and start targeted review.
A single malformed `Disposition: evidence-needed` marker follows the Writer path;
missing, duplicate, or unknown disposition markers remain an uncertain pause.

### Codex authentication pauses

Autonomous Codex workers use a private authentication home, not the interactive
Codex configuration or hooks. If Fluent pauses an Attempt for Codex
authentication, run `codex login`, then resume it with `fluent attempt run
<work-item-id> [attempt-id]`. Interactive sessions use the user's normal
configuration. For explicit skill roots or the autonomous Claude or OpenCode
home, read [`references/coder-environment.md`](references/coder-environment.md).

### Recover Claude readiness failures

Claude readiness errors use a stable `FLUENT_CLAUDE_*` code and a redacted diagnostic.
Run `claude /login` only for `FLUENT_CLAUDE_AUTH_REQUIRED`; use the scoped host-agent recovery above for `FLUENT_CLAUDE_HOST_CREDENTIAL_BLOCKED`.
For rate-limit, network, provider, timeout, or unknown codes, repair the named condition; do not substitute login.
Retry the same `fluent init` command, or resume an existing Task with `fluent attempt run <work-item-id> [attempt-id]` without consuming a Writer round. Upgrade versions older than 2.1.211 before parallel runs; the warning does not block a serial run.

For unrelated work that can proceed in parallel, create independent Work Items.

For codebase, module, or area review-only work, create a Work Item, run `fluent review codebase <work-item-id> <attempt-id>`, then `fluent attempt run <work-item-id> <attempt-id>`.

### Coder selection

`fluent attempt run` prints the resolved coder, model, and effort for each role
(writer and reviewer) before the first round. Before launching an
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
```

The `behavior-tests` role and flags are retired. Remove them from existing
configuration and scripts; Fluent rejects them instead of translating them.

Set a shared provider ceiling only in the user config:

```yaml
providers:
  concurrency:
    default: 5
    # claude: 3
```

A project can lower only its own provider use:

```yaml
providers:
  project-concurrency:
    default: 5
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

2. After the user chooses, ask which profile family to use, then wait:

   ```text
   Which coder setup should Fluent use?

   (a) Codex only — choose balanced or stronger next (recommended: balanced speed)
   (b) Claude only — claude-balanced; (c) OpenCode only — opencode-default
   (d) Mixed providers — choose the writer provider next
   (e) Custom — choose every writer and reviewer value
   ```

3. For Codex, ask `(a) codex-balanced — gpt-5.6-terra, medium` or
   `(b) codex-stronger — gpt-5.6-sol, medium`. For mixed, ask
   `(a) codex-write-claude-review` or `(b) claude-write-codex-review`. Wait for
   that answer. `claude-balanced` uses Claude's default model at medium effort.

4. For `custom`, explain that the writer implements the change and tests while
   reviewers evaluate both independently. Ask separately for each role's coder,
   model, and effort.

5. After all choices are complete, run one configured command:

   ```sh
   fluent init --coder-profile codex-balanced --follow-up-mode propose
   ```

   For `custom`, pass every `--write-*` and `--review-*` value. Explicit role
   flags may also override individual values in a curated profile.

   The coder profile controls Fluent's Writer and reviewers. It does not install
   the interactive Fluent skill into Codex or Claude Code directories; skill
   installation is separate. Use `fluent skills add --agent codex --project` or
   `fluent skills add --agent claude --global` for an explicit target.

6. Configured init checks durable Git paths, the host sandbox, and each distinct
   selected provider before mutation. For `FLUENT_INIT_DURABLE_PATH_IGNORED`,
   show its source, line, pattern, and affected path; ask the user to narrow the rule.
   Fluent's generated `.fluent/.gitignore` excludes runtime state such as `.fluent/work/`
   while retaining Expertise, hooks, and Tester configuration. Recover
   `FLUENT_HOST_SANDBOX_BLOCKED` through the scoped host-agent flow above. For
   provider failures, offer the user a retry or a different profile; do not
   confuse those login failures with the enclosing sandbox boundary above.
   These readiness checks do not promise future provider capacity.

7. After success, show the saved coder, model, and effort for both roles. Explain
   that new Attempts store them unless the user supplies explicit overrides.

`execute` authorizes and queues trusted corrective Work without starting it; a human runs `fluent scheduler run`, inspects the ready candidate, and lands it.

Read [usage-limit Writer recovery](references/host-evidence-recovery.md#recover-a-usage-limit-interrupted-writer) before approving a supported interrupted Writer; approval leaves it paused and requires a separate explicit resume.

## Writer testing contract

- `.fluent/tester.yaml` declares the project's test commands (one entry per harness, e.g., Rust nextest + shell).
- Each EARS statement has either a `Test:` reference pointing at a real test or an `Untestable:` marker with a one-line reason.
- Run focused harness-native selections before committing and report the exact
  commands, results, and covered risks. Leave the complete configured suite to
  Fluent's final Tester.
- Before declaring Writer completion, reconcile every Writer-owned matrix row
  and applicable finding with concrete implementation evidence and focused
  passing verification. Leave `host`, `review`, `tester`, and `learner` rows
  pending with empty evidence arrays. Do not defer a known implementation
  boundary, cleanup path, or navigation requirement to reviewers.

Fluent generates immutable rows from approved Behaviors and `Test:`/`Untestable:`
markers, Approach decisions, boundaries and guidance, required Plan rows and
Verification cells, and open findings. Use harness-native commands. Each finding
names an applicable Behavior, Approach, or required Plan constraint, or explains
why none applies. Incomplete Writer work follows the continuation or pause rules
above. Ordinary reviews require completed Writer work and any required host
evidence accepted by targeted Tests Review. Reviewers treat the matrix and focused
checks as advisory; only the Tests Reviewer may run one named missing check with
the candidate-keyed cache. Host, review, Tester, and Learner gates remain required.

Before ordinary review, the host writes `candidate-gate.json` for the exact Writer
commit using harness-native `Test:` references and the read-only `check-pre-merge`
hook. A rejection returns this artifact to the Writer for one focused correction;
reviewers receive it only after it passes.

After reviewers pass, the complete final Tester gates Learning on the exact
candidate. Established defects retain correction/repair routes; unmet prerequisites
need environment recovery, and unexplained failures pause for diagnosis.
For canonical host proof, follow status's `attempt evidence attach --required-check <id>`
command. Read [host evidence recovery](references/host-evidence-recovery.md)
for targeted Review, per-command replacement, aggregate blockers, and retries.

`fluent status` and `fluent work-item show <work-item-id>` also report cycle-cost
measurements: review rounds, completed-stage duration, local transcript tokens,
repeated findings, artifact bytes, and pre-review cycles avoided. Use these to
spot a Work Item whose context or review loop is growing before starting another
expensive round. These commands read one persisted sidecar instead of walking
artifact trees. After upgrading an existing project or manually repairing
artifacts, run `fluent work-item rebuild-metrics [work-item-id]` once.
Each Writer also stores `writer-context-usage.json` beside its transcript.
Inspect that sidecar when a correction is slow: it separates host prompt size,
transcript growth, cached replay, and uncached input so the next correction
targets the measured source of context growth.

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

During interactive stages, follow the stage references directly rather than calling these commands ad hoc. `references/plan-execution.md` is the one exception — it ends by running `fluent work-item create` as documented in its procedure. Read [mutable assignment context recovery](references/host-evidence-recovery.md#recover-mutable-writer-assignment-context) when permitted progress edits invalidate an older completed Writer’s or output-free completion retry’s saved input binding. Context recovery and retry authorization are separate actions; follow the next eligible command reported by status.
