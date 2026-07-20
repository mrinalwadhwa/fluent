
# Plan Execution

Interview the user. Decide whether this is one Work Item or several Work Items, then break each Work Item into steps the writer can follow. Once the plan is approved, create the Work Item(s).

## Read the inputs

Load these before starting the conversation:

- The confirmed brief at `.fluent/drafts/<draft-id>/brief.md`. The `<draft-id>` is set by `capture-brief`.
- The confirmed behaviors diff at `.fluent/drafts/<draft-id>/behaviors.diff.md`. Every EARS statement needs a home in a step of some Work Item's plan or an explicit TBD.
- The confirmed approach at `.fluent/drafts/<draft-id>/approach.md`. Its Open questions section lists anything execution must still resolve.
- The code the plan will touch — enough to see where each step lands, what depends on what, and any obvious ordering constraint.

If any of the three planning files is missing, stop and go back to the skill that produces it.

## Decide one Work Item or several

Inside a single Work Item, one writer produces the initial commit. Follow-up writes are that same writer responding to failed reviews, sequentially. There is no parallel implementation inside a Work Item. If two pieces should be built at the same time, they are two Work Items — each with its own Attempt, workspace, writer, reviewer, and Merge Candidate.

Splitting works when each piece is independently reviewable and shared interfaces can be pinned before another Work Item depends on them. A Work Item may block on a sync point, but the contract must be concrete enough that it knows when it can start or resume.

One Work Item is right when the pieces share vocabulary, iterate on each other's shape as writing proceeds, or must land together for a single reviewer pass.

Share your read:

> "One Work Item. The changes stay inside `dashboard/status.rs` — refactor the render loop and add the new invalidation handler in one pass. Does that match?"

Or when a split is on the table:

> "You raised two concerns today: the SSE server that emits `invalidated` events, and the dashboard subscriber that consumes them — different services, different reviewers, converging on the event schema. Split them? (a) two Work Items with the schema pinned first (recommended: the reviewers are independent and the contract is concrete enough to pin up front); (b) one Work Item, keeping the schema malleable across both. Which?"

If splitting, agree on each Work Item's slug now — short, kebab-case, unique within the draft. Handle each plan in the loop below.

## Check whether a plan is needed

Not every Work Item needs steps. A single mechanical change may need no steps at all:

> "The approach is one edit to `dashboard/status.rs` — emit the event and wire the subscriber. I don't see steps worth walking through. I'll write a minimal plan and move to Work Item creation. Sound right?"

Don't invent steps that aren't there. If the user agrees, go straight to Assemble and confirm.

## Walk through the steps

Steps are scaffolding for the writer: explicit states, referenced behaviors, and concrete verification. A step like "implement the SSE transport" is too coarse; "emit an `invalidated` event on the SSE endpoint for a hard-coded key and confirm the dashboard subscriber receives it" is right-sized.

The first step matters most: aim for a walking skeleton — the thinnest end-to-end slice that proves the approach works. Thin across every layer beats fat in one layer, because integration problems surface at the beginning rather than the end.

Propose the first step in the vocabulary of the approach, and describe what will be observably true when it's done:

> "First step: emit an `invalidated` event over SSE for a single hard-coded key, and confirm the dashboard receives it. That proves the transport and the subscriber wiring end to end. Right starting point?"

Then handle the rest one at a time or in small groups. For each step, agree on four things before moving on:

- The observable state the step reaches. "Users see cache-invalidation events on the status feed" is a state; "wire the subscriber" is an activity. Prefer the state form.
- The behaviors from `behaviors.diff.md` it satisfies, qualified by area since IDs restart per area — write `Feed:B1`, not `B1`. Every EARS statement must land in a step of some Work Item's plan or an explicit TBD.
- The verification — a test path, a curl command, a screen to check. Without this the writer doesn't know when to stop.
- Required or optional. Optional steps are the ones you'd trade away if execution runs into trouble.

After discussing each step or group of steps, ask:

> "Ordering hold? Reply **yes (y)**, or name a step to reorder or split."

If a step reveals a gap in the approach or the behaviors — a missing decision, a case that wasn't specified — stop and return to `design-approach` or `define-behaviors` rather than papering over it here.

If later steps depend on what execution will reveal in earlier ones, don't force detail. Plan through the visible horizon and mark the rest TBD:

> "I can plan through step 3. Step 4 depends on how the buffer behaves under a real subscriber — I'd mark it TBD and name that dependency."

## Name trades and risks

Before finalizing each Work Item's plan, surface two things:

- Scope trades — which optional steps get cut first if execution runs into trouble, in the order you'd cut them.
- Risks — assumptions the approach makes that you haven't verified, or dependencies with known limitations.

> "If things run tight, step 3 (buffer sizing) is the first thing I'd defer — the disconnected-subscriber case is rare. The risk I see: if SSE reconnection under load doesn't match the docs, we'd need to reconsider the transport."

## Pin the interfaces

When the plan splits into multiple Work Items, name where they meet before writing starts. Skip this section for a single Work Item.

Each meeting point produces two records: a sync point (when the Work Items converge) and an interface contract (what they exchange).

For each sync point, name which Work Items converge, what must be true at that point, and whether either blocks on it. Record these in the plan's `Sync points` section.

For each interface contract, name what's exchanged concretely — event schema, endpoint path, shared types, file paths. Record these in the plan's `Interfaces` section.

Parallel work diverges when contracts stay implicit.

> "Work Item `feed-server` owns the `invalidated` event shape and the endpoint path. Work Item `dashboard-subscriber` consumes them. The event schema and endpoint URL must be pinned before `dashboard-subscriber` can wire its consumer — blocking sync point for the client, non-blocking for the server."

## Assemble and confirm

For a single Work Item, write `plan.md` to `.fluent/drafts/<draft-id>/plan.md`.

For multiple Work Items, write one plan per Work Item to `.fluent/drafts/<draft-id>/items/<slug>/plan.md`. If a Work Item's scope would be muddied by unrelated content in the shared brief, behaviors, or approach, place a sliced version next to its plan under the same `items/<slug>/` directory. Otherwise, all Work Items share the top-level files.

Show the assembled plan(s) to the user:

> "Confirm the plan and move to Work Item creation? Reply **yes (y)**, or name what to revise: (a) a step, (b) a Work Item's scope, (c) a sync point."

Check that every behavior in the diff has a home, that verification is named for every step, and that the ordering respects the dependencies you found. When the plan splits, check that behaviors are partitioned across Work Items with no gaps or overlaps, and that pinned contracts match. If something needs changing, name which part — a specific step, a Work Item's scope, a sync point — and re-enter that step. Don't re-run the walkthrough.

Once the user confirms, move to Work Item creation.

## Create the Work Item(s)

For a single Work Item, `<work-item-id>` equals `<draft-id>`. Run:

```sh
fluent work-item create <work-item-id> \
  --title "<short title>" \
  --brief-file .fluent/drafts/<draft-id>/brief.md \
  --behaviors-file .fluent/drafts/<draft-id>/behaviors.diff.md \
  --approach-file .fluent/drafts/<draft-id>/approach.md \
  --plan-file .fluent/drafts/<draft-id>/plan.md
```

For multiple Work Items, run `fluent work-item create` once per Work Item. Each Work Item's `<work-item-id>` is `<draft-id>-<slug>`. Use its plan file at `.fluent/drafts/<draft-id>/items/<slug>/plan.md`. For brief, behaviors, and approach, use the Work-Item-specific file under `items/<slug>/` if it exists, otherwise the shared file at the draft root.

Do not create the Attempt or run it. That belongs to the autonomous stage in `fluent`. Stop here.

## Plan format

```markdown
# Plan

Draft id: [draft-id]
Brief: [one-line summary from the brief]

## Steps

| # | State reached | Behaviors | Verification | Req? |
|---|---------------|-----------|--------------|------|
| 1 | [observable state] | [Feed:B1, Feed:B2] | [test path or command] | Yes |
| 2 | [observable state] | [Feed:B3] | [test path or command] | Yes |
| 3 | [observable state] | [Dashboard:B1] | [test path or command] | Optional |
| 4+ | TBD — depends on [what step 3 reveals] | | | |

## Scope trades

1. [Step N] — [what's lost if cut]

## Risks

- [Risk and what it affects]

## Sync points

- [Which Work Items converge, what must be true, blocking or not]

## Interfaces

- [Contract with another Work Item — event shape, endpoint, types, file paths]
```

Omit sections with no content. A single-Work-Item plan has no Sync points or Interfaces. A minimal plan may contain only the Steps table, but every EARS statement in the behaviors diff still needs a row or an explicit TBD. A TBD row leaves Behaviors, Verification, and Req? blank — those are pinned during execution once earlier steps reveal the shape.

When the plan splits, each Work Item's plan opens with a header naming itself and the others:

```markdown
# Plan — [slug]

Draft id: [draft-id]
Work Item: [draft-id]-[slug]
Other Work Items: [other-slug], [other-slug]
Brief: [one-line summary from the brief]
```

## Rules

- Ask one question at a time, with a blank line after the question stem. Use two archetypes:
  - **Decision** — pick one option. Label the options (a)/(b)/(c), each self-contained; put the
    recommended option first and mark it `(recommended: <why>)`. The answer is a single letter.
  - **Confirm gate** — approve or route back: "Reply **yes (y)**, or name what to revise:
    (a).../(b).../(c)...". The default is yes; a bare `y` is accepted.
  Avoid the anti-pattern: an unlabeled "X or Y?" that forces the user to re-describe an option.
- Each step is a state the system reaches, not an activity to perform.
- Behavior references are area-qualified — `Feed:B1`, not `B1` — because IDs restart per area.
- When the plan splits, each Work Item's plan lives at `.fluent/drafts/<draft-id>/items/<slug>/plan.md` and its `<work-item-id>` is `<draft-id>-<slug>`.
