---
name: architect
description: >
  Make structural decisions through discussion with the user. Recognize
  when a decision is needed, frame options with trade-offs grounded in
  architectural principles, and document rationale. Use when implementation
  encounters a structural choice point — component boundaries, data
  ownership, dependency direction, abstraction level.
---

# Architect

Walk through structural decisions with the user one at a time. Each
decision follows the same cycle: frame the choice, evaluate options
against principles, choose, and record the rationale. The goal is
decisions that are understood, not just made.

This skill applies during implementation when a structural choice
emerges — not during upfront design (use design-approach) and not
during review (use review-architecture).

---

## How to run this skill

### Phase 1 — Load context

Read the architectural expertise:
- `expertise/architecture.md` — principles, viewpoints, anti-patterns

Read the system context:
- `documentation/architecture.md` — how the system is built today

If a run is active, read the approach and plan to understand what the
current work is trying to achieve.

### Phase 2 — Recognize the decision

Not every code change involves a structural decision. A decision exists
when:

- There are multiple valid ways to structure the code
- The choice affects what can change independently later
- The choice creates or moves a boundary between components
- The choice introduces or removes a dependency
- The choice affects who owns data or behavior

If there's no real structural choice — the implementation maps directly
to an established pattern — say so and move on. Don't manufacture
decisions.

### Phase 3 — Frame the decision

State the decision clearly before evaluating options:

> "We need to decide where X lives. This matters because it determines
> which components can change independently."

A well-framed decision includes:
- **What** needs to be decided
- **Why** it matters — what breaks, couples, or constrains if chosen
  poorly
- **What's at stake** — is this reversible or hard to undo?

Scope the decision. If it has sub-decisions, take the top-level one
first.

### Phase 4 — Evaluate options

For each viable option, assess against the relevant principles from
`expertise/architecture.md`. Not all principles apply to every
decision — use the ones that discriminate between the options.

For each option:
- **What it gives** — the benefit, the property it preserves
- **What it costs** — the trade-off, the constraint it introduces
- **Which principle it aligns with** — ground the evaluation in the
  expertise, not instinct

Use the architectural viewpoints to check blind spots. A decision
that looks clean from the development viewpoint may have problems
from the operational or deployment viewpoint.

Present options with their trade-offs:

> "Option A keeps the boundary clean — X and Y change independently.
> But it means duplicating the validation logic in both places.
>
> Option B shares the validation through a common module. Simpler
> now, but couples X and Y through the shared dependency. If their
> validation rules diverge later, that coupling becomes a problem."

Share which option you'd lean toward and why. Let the user decide.

### Phase 5 — Apply decision lenses

When the trade-off isn't clear-cut, apply these lenses selectively:

**Reversibility** — how hard is it to undo this decision? Easily
reversed decisions (rename a module, change an internal data
structure) deserve less deliberation. Hard-to-reverse decisions
(public API shape, database schema, component boundaries used by
other teams) deserve more.

**Change vectors** — what's most likely to change? Structure the code
so the most likely changes are easiest to make. If you don't know
what will change, keep options open — prefer the simpler structure
that's easy to restructure later.

**Inversion** — what would make this choice fail? Work backwards from
failure modes. If an option is fragile under likely conditions,
that's a signal.

**Second-order effects** — what does this decision make easier or
harder for future decisions? A choice that simplifies the immediate
problem but constrains future options may not be worth it.

### Phase 6 — Decide and record

Once the user chooses, record the decision. The record serves future
readers who encounter the code and wonder why it's structured this way.

A decision record captures:
- **Decision** — what was chosen
- **Context** — what prompted the decision
- **Options considered** — what the alternatives were
- **Rationale** — why this option won
- **Trade-offs accepted** — what was given up
- **Consequences** — what this decision means for future work

Record the decision in the appropriate place:
- If the decision changes the system's structure, update
  `documentation/architecture.md`
- If the decision is run-scoped, record it in the run's approach or
  a dedicated decisions section
- Code comments for decisions that would surprise a reader of the code

### Phase 7 — Revisit when context changes

Decisions are made with information available at the time. When new
information arrives — a requirement changes, a constraint shifts, an
assumption proves wrong — the decision may need to be revisited.

Signal when a previous decision should be reconsidered:

> "The original decision to share the validation module assumed X and Y
> would stay in sync. Now that Y needs different rules, we should
> revisit that coupling."

Don't revisit decisions without cause. Stable decisions should stay
stable.

---

## Rules

- **One decision at a time.** Don't bundle multiple structural choices
  into one discussion. Each decision gets its own frame-evaluate-choose
  cycle.
- **Ground in expertise.** Evaluate against the principles in
  `expertise/architecture.md`, not general instinct. Name the specific
  principle that applies.
- **Name what you give up.** Every choice has a trade-off. A
  recommendation without trade-offs is incomplete.
- **Show what was rejected.** The rationale for a decision is
  incomplete without knowing what it was chosen over. Record the
  alternatives.
- **Reversibility scales effort.** Spend time proportional to how hard
  the decision is to undo. Don't deliberate on easily reversible
  choices. Do deliberate on structural commitments.
- **Don't force decisions.** If the implementation maps to an
  established pattern with no meaningful alternatives, there's no
  decision to make. Say so and move on.
- **Fit the existing architecture.** Decisions should be consistent
  with `documentation/architecture.md`. If a decision changes the
  architecture, say so explicitly and update the documentation.
- **Record survives the conversation.** The decision record must make
  sense to someone who wasn't in the room. Write for the future reader,
  not the current participant.
