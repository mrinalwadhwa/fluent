---
name: design-approach
description: Interview the user to decide the technical approach for the defined behaviors and produce approach.md.
---

# Design Approach

Interview the user. Decide the technical direction that will deliver the behaviors — libraries, protocols, storage, structure, integration boundaries. The output is `approach.md` — direction to guide execution.

## Read the inputs

Load these before starting the conversation:

- The confirmed brief at `.factory/drafts/<draft-id>/brief.md`. The `<draft-id>` is set by `capture-brief`.
- The confirmed behaviors diff at `.factory/drafts/<draft-id>/behaviors.diff.md` — what the system must do. Its Open questions section lists the solution choices `define-behaviors` deferred to you.
- Existing architecture at `documentation/architecture.md` (if it exists) — the shape of the system today.
- `.factory/expertise/decisions.md` (if it exists) — recorded project choices. Any proposed direction must not contradict them; surface any conflict for the user to resolve.
- The code the new behaviors touch — enough to see the existing boundaries, patterns, and dependencies the approach will fit into or change.

Read `references/architecture.md` for the principles to evaluate structural choices against.

## Identify the decisions

Start with every Open questions item from the behaviors diff. Each one becomes a key decision here, moves to this approach's Open questions with a reason (research needed later), or gets dropped because research showed it's already settled.

Not every run needs deep design. A bug fix inside a settled area may have no real decisions. A new integration may have several. Before opening a conversation, list the choices the behaviors force:

- New external systems, protocols, or libraries to pick.
- Storage, transport, or serialization formats not already set.
- Boundaries that shift — a new component, a moved responsibility, a broken-out module.
- Places where the obvious pattern in the codebase doesn't obviously apply.

If nothing meaningful surfaces, say so to the user:

> "The behaviors map directly onto the existing status-line pattern. I don't see decisions worth walking through — I'll write a minimal approach that reuses `dashboard/status.rs`. Sound right?"

Don't invent decisions that aren't there. If the user agrees, go straight to Assemble and confirm.

## Research before proposing options

For any decision that turns on information you don't have, research before you talk to the user. Half-informed options waste their time.

Read the codebase for how similar concerns are handled today. When the choice touches an external system, look up its docs, auth model, data format, rate limits, and error responses. Stop once you can name the trade-offs. You don't need to become an expert in every dependency — you need enough to describe what each option gives and gives up.

If a decision needs research the user cares to see, say so before disappearing into it:

> "I don't know how the notification API handles reconnection. Give me a minute to read their docs, then I'll come back with the trade-off."

## Work decision by decision

Handle one decision per turn. For each, frame the choice, present the options with trade-offs, share your lean, and let the user pick:

> "For the status feed transport: (a) Server-Sent Events — one-way,
> reconnects automatically, works over plain HTTP, but no client-to-server
> messages; (b) WebSocket — bidirectional, but heavier and needs a fallback
> for proxies; (c) long-poll — simplest, but the dashboard sees events up
> to the poll interval late. I'd lean (a) since the dashboard only reads.
> Which?"

Name what each option gives up. A choice described only by its benefits reads like marketing.

If the user picks against your lean and you have a specific concern, name it before conceding.

When a decision feels off — too easy, too confident, stuck between two options — draw from the frameworks in `references/thinking.md`. Its *When to use which framework* table matches situations to tools. Describe the move, not the framework.

If the user rejects an option, ask what's wrong before revising. Don't re-propose the same option in different words. Move on when the decision is made. Don't revisit unless the user reopens it.

If a decision reveals a behavior is wrong or incomplete, stop and return to `define-behaviors` rather than designing around it.

## Discuss structure when it changes

A structural change — a new component, a moved responsibility, a shifted boundary — is a decision like any other, handled in the loop above. It differs only in how you present it: zoom out to the boundary first, and only zoom in where the choice depends on internal detail:

> "The status feed sits alongside the existing cache — the cache emits an
> invalidation event, the feed publishes it. That puts the transport on the
> feed side of the boundary, not the cache side. Does that match how you see
> it?"

Leave internal structure to the executing agent unless a specific piece has to be pinned down here. The approach names boundaries and interactions; it does not draw a class diagram.

## Assemble and confirm

Once every decision is agreed, write `approach.md` to `.factory/drafts/<draft-id>/approach.md` and show it to the user:

> "Here's the approach. Does the whole shape hold together, or does something
> feel off now that you see it side by side?"

Check that the vocabulary matches the behaviors diff, that no decision quietly contradicts a recorded choice in `.factory/expertise/decisions.md`, and that each key decision names what it gave up. If something needs changing, name which part — a specific decision, the structure section, or a risk — and re-enter that step. Don't re-run the whole walk-through.

Once the user confirms, stop. `plan-execution` picks up next.

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

- Label options as (a), (b), (c), or ask a yes/no with an obvious default. Avoid unlabeled "X or Y?" forms.
- Every choice names what it gives up. If nothing was given up, no real decision was made.
- Fit the existing architecture. If the approach changes it, say so explicitly and name what changes.
