---
name: design-approach
description: >
  Drive a conversation with the user to design a solution approach for
  the defined behaviors. Research external systems, evaluate options,
  make technical choices, and produce approach.md — direction for the
  executing agent that can evolve during implementation.
---

# Design Approach

Interview the user to design how the system should deliver the defined
behaviors. Work through decisions one at a time. Research what's needed,
surface options, discuss trade-offs. The document is assembled from the
conversation.

The output is `approach.md` — direction, not a commitment. The executing
agent has latitude to adapt. The real architecture gets captured in
`documentation/architecture.md` after implementation, reflecting what
was actually built.

---

## How to run this skill

### Phase 1 — Read the inputs

Read:
- `.factory/runs/[run-id]/brief.md` — the intent
- `.factory/runs/[run-id]/behaviors.diff.md` — what the system must do
- `documentation/architecture.md` — how the system is built today
- `expertise/architecture.md` — architectural principles for
  evaluating structural decisions
- Open questions deferred from define-behaviors

Understand the gap between the current architecture and what the new
behaviors require.

### Phase 2 — Identify decisions

Not every run requires deep design. A bug fix might need no design
decisions. A new integration might need several.

Scan the behaviors and identify the decisions that need to be made
before execution can begin:

- What components are involved?
- Are there external systems to integrate with?
- Are there multiple valid approaches?
- Does this change the system's boundaries or structure?
- What's new vs what's reusing existing patterns?

If there are no meaningful decisions — the behaviors clearly map to
existing patterns — say so and write a minimal approach.md. Don't
manufacture complexity.

### Phase 3 — Research (when needed)

For decisions that need information you don't have, research before
discussing with the user. Use codebase research for internal patterns
and internet research for external systems.

Research as needed:
- External APIs, protocols, and libraries
- Authentication and authorization models
- Data formats, rate limits, error responses
- How the existing codebase handles similar concerns
- Evolution stage: is this component novel (build), established (use
  existing library), or commodity (use a managed service)?

The goal is to know enough to present informed options, not to become an
expert in every dependency. Stop when you can describe the trade-offs.

### Phase 4 — Walk through decisions with the user

Take each decision one at a time. For each:

1. **Frame the decision:**
   > "For handling X, there are a couple of approaches. Let me walk
   > through them."

2. **Present options with trade-offs.** Not "option A is better" but
   "option A gives us X but costs Y; option B gives us Z but costs W."
   Name what each option gives up.

3. **Share what you'd lean toward and why**, but let the user choose.
   Don't bury decisions in assumptions.

4. **Move on** when the decision is made. Don't revisit unless the user
   brings it up.

Use these lenses selectively:

**First Principles** — when the obvious approach might be convention
rather than the right choice. "Is this actually the best fit here, or
just how it's usually done?"

**Inversion** — when risks feel underweighted. "What would make this
approach fail?" Work backwards from failure modes.

**WYSIATI** — when the decision feels too easy. "What are we not seeing
that could change this choice?"

**Boundary thinking** — where does core domain logic end and external
adapters begin? What should the system own vs delegate? This affects
how the system can evolve — things behind boundaries can change
independently.

**Integration patterns** — when connecting to external systems, how
should the boundary work? Adopt their model (conformist)? Translate at
the boundary (anti-corruption layer)? Publish a shared format?

**Build vs use vs buy** — is this component novel enough to build, or
is there an existing library or managed service? Novel components need
investment; commodity components should be adopted, not reinvented.

### Phase 5 — Discuss system structure (when the run changes it)

If the behaviors require new components, new boundaries, or changes to
how existing components interact, walk through the structural changes
with the user.

Use levels of abstraction — start zoomed out, zoom in where it matters:

- **Context** — what systems interact? What's external?
- **Containers** — what runtime components? (APIs, databases, queues,
  services)
- **Components** — how are things organized internally? (Only go here
  if the decision depends on internal structure.)

> "The new behavior means we need a separate service for X, talking to
> Y through Z. Does that match your mental model?"

Don't over-specify internal structure. The executing agent will make
those decisions. Focus on the boundaries and interactions that the user
needs to agree on.

### Phase 6 — Assemble and confirm

Once all decisions have been discussed, assemble `approach.md` and show
it to the user:

> "Here's the approach based on our discussion. Does it capture the
> decisions we made? Anything feel off?"

This is a coherence check. If something needs changing, fix it and
confirm again.

Set status to `approach-designed`.

---

## Output format

```markdown
# Approach

Run: [run-id]
Brief: [one-line summary]

## Key decisions

### [Decision 1]
Choice: [what was chosen]
Why: [rationale]
Alternatives: [what was considered and why not]
Trade-offs: [what this choice gives up]

### [Decision 2]
...

## Solution outline

[How the system delivers the new behaviors. Components, interactions,
boundaries — enough to guide execution. Not a detailed design.]

## Risks

- [Risk and how the approach accounts for it]

## Open questions

- [Anything to resolve during execution]
```

---

## Rules

- **Interview, don't present.** Walk through decisions one at a time.
  Don't produce a document and ask for approval. This applies to the
  full conversation — decisions, solution outline, and risks each get
  their own discussion. Don't walk through decisions carefully and
  then dump the rest as one block.
- **Direction, not commitment.** approach.md is a hypothesis. The
  executing agent can adapt it. The real architecture lands in
  architecture.md after implementation.
- **Renegotiation is expected.** If the executing agent discovers the
  approach doesn't work, they propose changes via `needs-user`. This
  is normal.
- **Research before options.** Don't present options you haven't
  researched. Half-informed options waste the user's time.
- **Name the trade-offs.** Every choice gives something up. Say what.
- **Show what was rejected.** The rationale for a choice is incomplete
  without knowing what it was chosen over.
- **Don't manufacture decisions.** If the behaviors clearly map to
  existing patterns, say so. A minimal approach is fine. Not every
  run needs deep design.
- **Fit the existing architecture.** The approach should be consistent
  with `documentation/architecture.md`. If it changes the architecture,
  say so explicitly.
- **Don't over-specify.** Describe boundaries and interactions. Leave
  internal structure to the executing agent.
- **Vocabulary carries forward.** Use the terms established in
  behaviors.diff.md. The same nouns should appear in the approach.
