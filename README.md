# Fluent – a self improving software factory

Fluent is a factory that autonomously turns your team's vision, ideas, bug reports, user feedback, production logs, and agent traces into working software.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/assets/fluent-at-a-glance-dark.gif">
    <source media="(prefers-color-scheme: light)" srcset=".github/assets/fluent-at-a-glance-light.gif">
    <img alt="The Fluent skill lets you drive the factory by chatting with Codex, Claude Code, or another coding agent. Agent traces, production logs, user feedback, and team vision and ideas become Observations. Conversation shapes Work Items that a scheduler delegates through Writer, Tester, Reviewers, and Learner. Review findings return to the Writer; Expertise guides future work; and new findings return as Observations. You or an opt-in policy can authorize landing. After merge, Fluent can watch deployment jobs and production activity. Delegated work can run locally or remotely, with people pulled in for context and decisions." src=".github/assets/fluent-at-a-glance-light.gif" width="100%">
  </picture>
</p>

You use the factory through the Fluent [skill](https://agentskills.io/) in Codex, Claude Code, Pi, or another coding agent that supports skills. The skill turns your conversation into Fluent's interface and drives its command-line machinery for you.

### Install

```bash
npx skills add mrinalwadhwa/fluent --skill fluent  # macOS and Linux
```

Use the command above to install the Fluent skill. Start your coding agent in the project folder you want Fluent to work on, then ask it to use Fluent and describe what you want to explore, build, fix, or improve.

The first invocation sets up Fluent for that project and starts shaping the work with you. You can also invoke the skill explicitly: `$fluent` in Codex, `/fluent` in Claude Code, or `/skill:fluent` in Pi.

## How Fluent works

Fluent separates work that needs human attention from work agents can do on their own. You can think of it as two conceptual queues. The first queue waits for people with the right context, judgment, expertise, or authority. The second waits on both agent and compute capacity: room to run an agent within subscription, rate, and budget limits, and a suitable environment with the models, tools, and hardware the work needs.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/assets/fluent-overall-flow-dark.png">
    <source media="(prefers-color-scheme: light)" srcset=".github/assets/fluent-overall-flow-light.png">
    <img alt="Two conceptual queues let human attention and delegated work advance independently. Observations, needs-user handoffs, and Merge Candidates can wait for human attention. Work Items, Attempts, and Merge Candidates can wait for agent and compute capacity. New Observations and agent handoffs feed the first queue; shaped work, answers, and approvals feed the second." src=".github/assets/fluent-overall-flow-light.png" width="100%">
  </picture>
</p>

Whenever you encounter something to explore, build, fix, or improve, ask Fluent to record it as an Observation. Add whatever context you have in the moment; you do not need to know the solution or have worked out every detail. Agents and connected systems can record Observations too. You can return to any Observation later and refine it on your own timeline, bringing in someone with the right expertise or authority when needed.

When you want to act on an Observation, ask Fluent to start shaping it. Once shaping produces a Work Item, ask Fluent to run it directly or add it to the queue. A queued Work Item waits until suitable agent and compute capacity is available. If its Attempt needs human context or a decision, Fluent places the question in the human queue for you or someone with the right expertise or authority to answer. The Attempt waits for that answer while other ready Work Items can continue.

## How you tell Fluent what to build

Ask Fluent to help shape what you want to build. You can start from an Observation you recorded earlier or begin directly with a Brief. Fluent reads the relevant project code along with the reusable conventions, constraints, and lessons captured as project Expertise.

You do not need to arrive with a finished specification. Fluent collaborates with you as the slice takes shape. It grounds the conversation in the code and project Expertise, checks that it understands what you mean, and asks one focused question at a time. It uses structured methods for problem framing, behavior design, architecture, and planning to challenge assumptions, find missing cases, research technical choices, and present options with their tradeoffs. You provide context and judgment and make each decision.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/assets/how-you-tell-fluent-dark.png">
    <source media="(prefers-color-scheme: light)" srcset=".github/assets/how-you-tell-fluent-light.png">
    <img alt="You and Fluent shape a slice in conversation. An earlier Observation can feed a Brief, followed by Behavior Specifications, a Technical Approach, and an Implementation Plan. Fluent offers focused questions and options; you add context, judgments, and decisions. Gaps return to an earlier layer, while an approved Implementation Plan becomes one or more Work Items." src=".github/assets/how-you-tell-fluent-light.png" width="100%">
  </picture>
</p>

The conversation produces four layers of shared context:

**Brief.** A Brief describes one small slice of functionality. Fluent is designed to help you turn that slice into working software without first specifying the entire system. The Brief captures, in your words, what you want and why, grounded in the relevant project context. It keeps constraints, assumptions, and unknowns explicit without choosing a solution.

**Behavior Specifications.** A Behavior Specification precisely describes how the software must behave in a particular situation. It is written so the specified observable behavior can be verified without prescribing its implementation.

For the slice described in the Brief, Fluent reads the project’s existing Behavior Specifications and relevant code to understand what the software already guarantees. It then works with you to define the additions, changes, or removals needed for that slice. The result is a behavior diff, not a restatement of the entire system. If the project has no existing Behavior Specifications, the first slice starts them.

Before writing Behavior Specifications, Fluent makes the important terms precise and maps the people and systems involved, the events that occur, and the states that matter. For a small slice, it considers only what changes. It works through one area at a time, proposing a few core behaviors before asking about gaps and important edge cases. If Fluent derives a behavior that was not stated in the Brief, it labels the behavior as derived so you can accept or reject it. Decisions about libraries, protocols, storage, and other solution choices wait for the Technical Approach.

Fluent writes each Behavior Specification in a consistent form that names the situation and the required response. For example:

```text
WHEN a user selects Save on a draft,
THE SYSTEM SHALL show a `Saved` status beside the draft title.
Test: tests/drafts.spec.ts (shows_saved_status_after_save)
```

It describes something a person can observe and a test can verify, without choosing a UI framework, component structure, or persistence mechanism.

Fluent writes Behavior Specifications in EARS, the Easy Approach to Requirements Syntax. EARS uses a small set of patterns that make the triggering event or condition and the required response explicit.

Every new behavior includes either a `Test:` reference or an `Untestable:` reason. While defining the behavior, Fluent inspects nearby tests and names the intended test in the project’s existing style. The referenced test may not exist yet. The Writer creates or updates it during implementation, and the Tester runs it. When the test passes, the Behavior Reviewer uses that as evidence that the behavior was delivered.

**Technical Approach.** The Technical Approach document captures your technical expertise and judgment before the work is delegated to agents. You and Fluent decide the key technical choices that should guide implementation, including structure, interfaces, protocols, libraries, storage, and integrations. The document gives the Writer both the decisions and the reasoning behind them, while leaving implementation details that agents can safely determine during the work.

**Implementation Plan.** The Implementation Plan turns the confirmed behaviors and technical decisions into work that agents can carry out. You and Fluent decide whether the slice should become one Work Item or several independently reviewable Work Items that can run in parallel. When steps help the Writer, the Implementation Plan divides a Work Item into steps that state what will become observably true, which behaviors each step delivers, and how the result will be verified. Fluent orders those steps by what must be built first. A small mechanical Work Item can use a minimal plan without inventing steps. When Work Items depend on one another, the plan pins the interfaces they share and the points at which their work must come together.

You confirm each layer before Fluent uses it as the foundation for the next. The conversation can also move backward. If the Technical Approach reveals missing behavior, you return to the Behavior Specifications. If the Implementation Plan exposes an unresolved technical decision, you return to the Technical Approach rather than leaving the Writer to guess.

When you confirm the Implementation Plan, Fluent creates one Work Item for each independently reviewable part of the slice. Each Work Item carries the approved Brief, Behavior Specifications, Technical Approach, and the part of the Implementation Plan its agents must deliver. Creating a Work Item records the handoff. It does not run or queue the Work Item.

Ask Fluent to run a Work Item directly, or add it to the queue to wait for agent and compute capacity. Each time Fluent runs a Work Item, it creates an Attempt. The Attempt tracks that execution through the Writer, Tester, Reviewers, and Learner, including every revision, test run, review, and pause. Separate Work Items can proceed independently. Within an Attempt, the Writer follows any dependencies and ordering recorded in the Implementation Plan and tracks its progress in `progress.md`.

## How Fluent builds it

Fluent gives each Attempt an isolated Git worktree, keeping its changes separate from your working copy. It can run the Writer, Tester, Reviewer, and Learner tasks in a local sandbox or delegate them to a remote machine, currently through AWS Fargate. If the Attempt needs human context or a decision, Fluent returns the question to the human queue.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/assets/how-fluent-builds-dark.png">
    <source media="(prefers-color-scheme: light)" srcset=".github/assets/how-fluent-builds-light.png">
    <img alt="One Attempt runs a Work Item through the Writer, an independent Tester, parallel Reviewers, and the Learner to produce a ready Merge Candidate. Tester regressions and blocking review findings return to the Writer. The Attempt can pull in a human for context or decisions and resume with additional human input. It can run in local sandboxes or on remote machines." src=".github/assets/how-fluent-builds-light.png" width="100%">
  </picture>
</p>

Within the Attempt, the Writer receives the approved Brief, Behavior Specifications, Technical Approach, and its assigned part of the Implementation Plan, along with the relevant code, project Expertise, and instructions. It implements the change, writes or updates its tests, and commits a candidate for the Tester and Reviewers.

Fluent does not rely on the Writer’s own test report, and it does not make every Reviewer rerun the same test suite. Instead, a separate Tester produces one independent test record that all Reviewers share.

The Tester is a deterministic runner, not a coding agent. It reads one or more test commands from the project’s `.fluent/tester.yaml`, runs them sequentially in the candidate workspace, and captures each command’s output, exit code, and duration.

A command can declare the paths it exists to exercise, and then runs only for a candidate that touches them:

```yaml
commands:
  - command: ./scripts/explain-new-indexes
    test_harness: shell-harness
    when_changed:
      - migrations/**
      - "**/*.sql"
```

This is how a project attaches a gate to a file class that its main suite cannot judge — a chart template, a compose file, a migration whose index the query cannot use. Without it, such a change lands green on the strength of checks that never looked at it. A command with no `when_changed` runs every time, and when the Tester cannot determine what changed it runs every command. Commands the diff did not reach are recorded in `tester-results.json` as skipped, so a Reviewer can tell a gate that passed from one that never ran. The project’s `.fluent/extract-tester-results` executable normalizes those outputs into a single `tester-results.json` artifact. Reviewers treat that artifact as authoritative for the project’s test suite, although they can still run targeted checks for specific questions. The Behavior Reviewer also matches the `Test:` references in the Behavior Specifications against its per-test results.

Once the Tester completes, Fluent runs independent Reviewers in parallel. Every Reviewer reads the approved planning documents, the Writer’s `progress.md` checklist, the candidate changes, the Tester evidence, and recorded decisions. The checklist tracks any Implementation Plan steps and review follow-ups across rounds, recording completion evidence, divergences, and notes for what comes next. Each Reviewer also loads relevant general and project Expertise, so its judgment reflects both Fluent’s review methods and what the project has learned. The built-in Reviewers include:

- The Behavior Reviewer checks both the Behavior Specifications and whether the candidate delivers them. It looks for statements that combine multiple effects, prescribe implementation, have more than one interpretation, contradict one another, omit intended or failure behavior, or lack a passing test.
- The Architecture Reviewer evaluates the change in the context of the whole codebase. It looks for the simplest maintainable structure, clear responsibilities and boundaries, appropriate abstractions and dependencies, unnecessary coupling, known structural anti-patterns, and departures from the approved Technical Approach or recorded decisions. It considers information flow, concurrency, deployment, and operations when they matter to the change.
- The Tests Reviewer checks whether the tests provide enough confidence to ship and refactor safely. It looks for uncovered behavior, boundaries, and error paths; tests coupled to internal structure; tests that do not exercise the actual code; poor choices of test level or doubles; shared state, nondeterminism, slowness, and failures that are hard to diagnose. It also challenges every `Untestable:` justification.
- The Documentation Reviewer checks whether the change is documented at the levels its readers need, from code comments and module documentation to architecture and user-facing guides. It reads the documentation alongside the code to find missing or inaccurate explanations, inconsistent vocabulary, poor organization, vague claims, and prose that does not help someone understand or use the system.
- The Skills Reviewer reads each changed Agent Skill as a complete package and traces how it is invoked. It checks the skill’s specification, discoverability, progressive disclosure, instructions, references, scripts, assets, writing, interactive questions, and overlap with sibling skills, then asks whether the package is likely to accomplish its stated purpose.

Each Reviewer writes a report that records its evidence, classifies each finding as blocking or minor, and returns `pass`, `fail`, or `uncertain`. `pass` means no blocking findings remain. `fail` means the Writer must address at least one blocking finding. `uncertain` means the Reviewer cannot confidently decide from the approved context and returns the question to the human queue.

After a `fail`, Fluent starts another round. The Writer receives the failed review reports and any Tester regressions, revises the candidate, and commits the revision. The Tester reruns all declared test commands, then the affected Reviewers inspect the revision and mark earlier findings as addressed or still open. If the Attempt reaches its configured review-round limit, Fluent pauses with the evidence collected rather than continuing indefinitely.

Once the Tester and every Reviewer pass, the Learner examines the accepted change and the evidence from every round, updates Expertise when it finds something reusable, and records possible follow-ups in a handoff. The Attempt then produces a ready Merge Candidate.

A ready Merge Candidate contains the reviewed change and any Expertise captured by the Learner, but it has not changed the target branch. Ask Fluent to show you the candidate so you can inspect it and decide whether to land it. By default, Fluent waits for your decision. Projects that want automatic landing can opt into a separate auto-merge process.

To land the candidate, Fluent first checks that both the candidate and target worktrees are clean. It rebases the candidate onto the current target branch, asking an agent to resolve straightforward conflicts and returning unresolved ones to you. It then runs the project’s optional `check-pre-merge` hook. If the check fails and a `fix-pre-merge` hook exists, Fluent runs it, commits any correction it makes, and checks again. Fluent moves the target branch only after these steps pass, using a fast-forward merge.

## How Fluent keeps improving your code

While working on a Work Item, Fluent can identify related problems and possible improvements beyond the Brief it was asked to deliver. Problems with the current change, such as Tester regressions and blocking review findings, return to the Writer inside the current Attempt. The Learner records the additional improvements as follow-ups in its handoff. After the candidate lands, Fluent turns each follow-up into an Observation that you can shape and run later.

You can return to one of these Observations later and shape it through the usual conversation. If the Learner captured a complete, testable correction grounded in a current Behavior Specification, project instruction, or project Expertise, Fluent can turn the Observation into a corrective Work Item without repeating the full shaping process. If what it changes, how it will be verified, the authority it follows, or any required decision remains unclear, the Observation waits for you to shape it.

When Fluent creates one of these corrective Work Items, what happens next depends on the follow-up mode you chose during project setup. In the default `propose` mode, the Work Item waits for you to authorize it. In `execute` mode, Fluent authorizes and queues it automatically, provided the chain of follow-up work remains within the project’s configured limit. Once authorized and queued, a running scheduler picks up the Work Item when the required agent and compute capacity are available. It then goes through the same Writer, Tester, Reviewer, and Learner loop, and its Merge Candidate follows the same inspection and landing policy as any other.

Fluent can also inspect code after it lands. Post-merge review is optional and off by default. When you enable it for a land, Fluent waits briefly so nearby merges can be reviewed together, then runs the Tester and every Reviewer against the cumulative change on the target branch. If a Reviewer fails or is uncertain, Fluent creates and immediately runs a corrective Work Item using the findings as context. Any Merge Candidate it produces follows the same inspection and landing policy as any other candidate.

## How Fluent learns your project

Each completed change can teach Fluent something about the project: a convention to follow, an architectural constraint to preserve, a testing pattern to reuse, or a gotcha to avoid. The Learner records reusable lessons as Expertise under `.fluent/expertise/`. Fluent draws on that Expertise when it shapes, builds, and reviews future work.

After the Tester and every Reviewer pass, the Learner examines the accepted change, existing Expertise, and the Tester and Reviewer evidence from every round. The Learner updates Expertise when the Attempt reveals project-specific knowledge that future agents should reuse. If the Attempt taught Fluent nothing worth carrying forward, Expertise remains unchanged.

Any Expertise the Learner adds becomes part of the Merge Candidate and lands with the change that produced it. Over time, lessons from many Work Items accumulate in project Expertise, where future shaping conversations, Writers, and Reviewers can build on them. You can inspect and edit Expertise directly when a convention changes or a recorded lesson no longer applies.

Each completed change improves the project and shapes how Fluent approaches future work.

---

👉 [Join our conversation on building software factories, on Discord.](https://discord.gg/dygEVzG3gf)
