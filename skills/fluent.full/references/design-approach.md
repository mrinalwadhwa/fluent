
# Design Approach

Interview the user. Decide the technical direction that will deliver the behaviors — libraries, protocols, storage, structure, integration boundaries. The output is `approach.md` — direction to guide execution.

When planning-wide delegation is active, apply the Fluent skill's delegation
contract throughout this stage. Make grounded recommendation-level choices and
record their alternatives and tradeoffs. A mandatory interruption still stops
the stage for one focused user decision.

## Read the inputs

Load these before starting the conversation:

- The confirmed brief and behaviors diff, or read provisional planning inputs
  from `.fluent/drafts/<draft-id>/brief.md` and `behaviors.diff.md` when
  planning-wide delegation is active. The `<draft-id>` is set by `capture-brief`.
  The behavior diff's Open questions section lists the solution choices
  `define-behaviors` deferred to you.
- Existing architecture at `documentation/architecture.md` (if it exists) — the shape of the system today.
- `.fluent/expertise/decisions.md` (if it exists) — recorded project choices. Any proposed direction must not contradict them; surface any conflict for the user to resolve.
- The code the new behaviors touch — enough to see the existing boundaries, patterns, and dependencies the approach will fit into or change.

Read `references/architecture.md` for the principles to evaluate structural choices against.

## Identify the decisions

Start with every Open questions item from the behaviors diff. Each one becomes a key decision here, moves to this approach's Open questions with a reason (research needed later), or gets dropped because research showed it's already settled.

Not every run needs deep design. A bug fix inside a settled area may have no real decisions. A new integration may have several. Before opening a conversation, list the choices the behaviors force:

- New external systems, protocols, or libraries to pick.
- Storage, transport, or serialization formats not already set.
- Boundaries that shift — a new component, a moved responsibility, a broken-out module.
- Places where the obvious pattern in the codebase doesn't obviously apply.

If nothing meaningful surfaces without planning-wide delegation, say so to the
user:

> "The behaviors map directly onto the existing status-line pattern. I don't see decisions worth walking through — I'll write a minimal approach that reuses `dashboard/status.rs`. Sound right?"

Don't invent decisions that aren't there. Without delegation, if the user agrees,
go straight to Assemble and confirm. With delegation active, record the reused
pattern and continue without asking.

## Research before proposing options

For any decision that turns on information you don't have, research before you talk to the user. Half-informed options waste their time.

Read the codebase for how similar concerns are handled today. When the choice touches an external system, look up its docs, auth model, data format, rate limits, and error responses. Stop once you can name the trade-offs. You don't need to become an expert in every dependency — you need enough to describe what each option gives and gives up.

If a decision needs research the user cares to see, say so before disappearing into it:

> "I don't know how the notification API handles reconnection. Give me a minute to read their docs, then I'll come back with the trade-off."

## Work decision by decision

Without planning-wide delegation, handle one decision per turn. For each, frame
the choice, present the options with trade-offs, put the one you recommend first
and mark it `(recommended: <why>)`, and let the user pick:

> "For the status feed transport: (a) Server-Sent Events — one-way,
> reconnects automatically, works over plain HTTP, but no client-to-server
> messages (recommended: the dashboard only reads); (b) WebSocket —
> bidirectional, but heavier and needs a fallback for proxies; (c) long-poll
> — simplest, but the dashboard sees events up to the poll interval late.
> Which?"

Name what each option gives up. A choice described only by its benefits reads like marketing.

When planning-wide delegation is active, choose the supported recommendation,
record the alternatives and what the choice gives up, and continue without an
intermediate question. A consequential or difficult-to-reverse choice, a
conflict with a recorded project decision, or a choice that requires unavailable
information is a mandatory interruption. After the user resolves it, continue
delegated planning unless they revoke delegation.

If the user picks against your recommendation and you have a specific concern, name it before conceding.

When a decision feels off — too easy, too confident, stuck between two options — draw from the frameworks in `references/thinking.md`. Its *When to use which framework* table matches situations to tools. Describe the move, not the framework.

If the user rejects an option, ask what's wrong before revising. Don't re-propose the same option in different words. Move on when the decision is made. Don't revisit unless the user reopens it.

If a decision reveals a behavior is wrong or incomplete, stop and return to
`define-behaviors` rather than designing around it. Changing that behavior is a
mandatory interruption even when planning-wide delegation is active.

## Discuss structure when it changes

A structural change — a new component, a moved responsibility, a shifted boundary — is a decision like any other, handled in the loop above. It differs only in how you present it: zoom out to the boundary first, and only zoom in where the choice depends on internal detail:

> "The status feed sits alongside the existing cache — the cache emits an
> invalidation event, the feed publishes it. That puts the transport on the
> feed side of the boundary, not the cache side. Does that match how you see
> it?"

Leave internal structure to the executing agent unless a specific piece has to be pinned down here. The approach names boundaries and interactions; it does not draw a class diagram.

## Assemble and confirm

Without planning-wide delegation, once every decision is agreed, write
`approach.md` to `.fluent/drafts/<draft-id>/approach.md` and show it to the user:

> "Confirm the approach and move to planning? Reply **yes (y)**, or name what to revise: (a) a decision, (b) structure, (c) a risk."

Check that the vocabulary matches the behaviors diff, that no decision quietly contradicts a recorded choice in `.fluent/expertise/decisions.md`, and that each key decision names what it gave up. If something needs changing, name which part — a specific decision, the structure section, or a risk — and re-enter that step. Don't re-run the whole walk-through.

Once the user confirms, stop. `plan-execution` picks up next.

When planning-wide delegation is active, write `approach.md` as provisional,
give a concise progress update without asking for approval, and continue directly
to `plan-execution`. Do not call any planning artifact approved. The approach
remains subject to the one final planning-set confirmation, and a requested
revision must update it and every affected downstream artifact.

## Approach format

```markdown
# Approach

Draft id: [draft-id]
Brief: [one-line summary from the brief]

## Key decisions

### [Decision]
Choice: [what was chosen]
Why: [the reason it fits]
Alternatives: [what was considered and why not]
Trade-offs: [what this choice gives up]

### [Decision]
...

## Structure

[The components involved, how they interact, and where the boundaries
sit. Enough to guide execution, not a full internal design.]

## Execution guidance

- [Expertise files, docs, or code patterns execution should follow]

## Risks

- [Risk and how the approach accounts for it]

## Open questions

- [Anything left for execution to resolve]
```

Omit sections with no content. A minimal approach for a mechanical change may be Key decisions only, or a single sentence under Structure pointing at the pattern being reused.

## Rules

- Ask one question at a time, with a blank line after the question stem. Use two archetypes:
  - **Decision** — pick one option. Label the options (a)/(b)/(c), each self-contained; put the
    recommended option first and mark it `(recommended: <why>)`. The answer is a single letter.
  - **Confirm gate** — approve or route back: "Reply **yes (y)**, or name what to revise:
    (a).../(b).../(c)...". The default is yes; a bare `y` is accepted.
  Avoid the anti-pattern: an unlabeled "X or Y?" that forces the user to re-describe an option.
- Planning-wide delegation is the only exception to the question-by-question
  rule: after an explicit request, do not ask recommendation-level questions or
  intermediate confirmation gates; keep mandatory interruptions and the final
  planning-set confirmation.
- Every choice names what it gives up. If nothing was given up, no real decision was made.
- Fit the existing architecture. If the approach changes it, say so explicitly and name what changes.
