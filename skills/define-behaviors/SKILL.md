---
name: define-behaviors
description: >
  Drive a conversation with the user to define the observable behaviors
  for a piece of work. Establish the domain vocabulary, map the behavioral
  space, and produce behaviors.diff.md — an incremental set of EARS-format
  statements that extend the project's existing behaviors.
---

# Define Behaviors

Interview the user to define what the system should do — observable
outcomes, not implementation. Work in small pieces: one area at a time,
one behavior at a time. The document is assembled from the conversation,
not presented as a finished artifact.

The output is `behaviors.diff.md` — an increment over what already exists
in `documentation/behaviors.md`, not a restatement.

---

## How to run this skill

### Phase 1 — Read the inputs

Read:
- The approved brief from the active planning conversation or draft
  artifact — the normal source of intent before `factory work create`
  stores Work Item planning context
- Work Item planning context from `factory work show <work-item-id>` only
  when the Work Item already exists
- `.factory/runs/[run-id]/brief.md` only in a legacy fallback or
  recovery path
- `documentation/behaviors.md` — what the system already does
- Relevant code in the areas the brief describes

Understand the gap between what exists and what the brief asks for.
Identify unknowns from the brief that need resolution.

If anything in the brief is too vague to elaborate, ask the user before
proceeding. Do not invent intent.

### Phase 2 — Establish vocabulary

Before defining behaviors, pin down what things are called. The brief
introduces terms loosely — this is where they get precise.

Ask the user:
- "When you say X, what exactly do you mean?"
- "Is this the same thing as Y in the existing system, or different?"
- "What does the user call this? What would they see in the UI?"

If the project already has a vocabulary (in `documentation/behaviors.md`
or `documentation/architecture.md`), use it. Introduce new terms only
when the brief describes something genuinely new. Note new vocabulary
explicitly — it becomes part of the project's shared language.

Keep this short. A few key terms, not a glossary. Move on when the core
nouns and verbs are clear enough to write behaviors.

### Phase 3 — Map the behavioral space

Before writing individual statements, sketch the territory with the
user. Identify:

- **Actors** — who or what interacts with the system? (user, admin,
  external service, scheduler, other system component)
- **Events** — what happens? What triggers behavior? (user actions,
  API calls, time-based events, system events)
- **States** — what ongoing conditions matter? (logged in, processing,
  offline, rate-limited)

Walk through this with the user one dimension at a time:

> "The actors I see are the user and the external API. Are there others?"

> "The main events seem to be: user submits, API responds, timeout
> occurs. What am I missing?"

This maps naturally to EARS patterns — events become WHEN triggers,
states become WHILE conditions, actors clarify who's involved. Don't
write EARS statements yet. Just agree on the landscape.

### Phase 4 — Define behaviors area by area

Work through one area at a time. For each area:

1. **Propose a few core behaviors** — the ones clearly stated or implied
   by the brief. Show them to the user immediately. Keep it short.

2. **Ask about each one:**
   > "Does this capture what you mean? Is the wording right?"

3. **Ask about gaps — one at a time:**
   > "What should happen if X fails?"
   > "What about when Y is empty or invalid?"

4. **Ask about implicit behavior:**
   Only surface things that might be ambiguous or where the project
   might depart from convention. Do not enumerate every obvious behavior.
   Most software conventions (error messages on invalid input, loading
   states during async operations) are implicit — the user expects them
   without stating them. Only call them out when:
   - The convention is unclear for this specific case
   - The project explicitly does something different
   - The brief's context makes the default ambiguous

5. **Move to the next area** when the user confirms this one is right.

Use EARS notation for each statement:

| Pattern | Template | Use when |
|---|---|---|
| Ubiquitous | The system shall [behavior] | Always true |
| Event-driven | WHEN [event], the system shall [behavior] | Triggered by an event |
| State-driven | WHILE [state], the system shall [behavior] | True during a condition |
| Unwanted | IF [condition], THEN the system shall [behavior] | Handling failures |
| Optional | WHERE [feature], the system shall [behavior] | Configurable |
| Complex | WHILE [state], WHEN [event], the system shall [behavior] | Compound |

Where it helps, include a concrete example alongside the EARS statement
to make the behavior precise: "For example, when a user submits a form
with an empty email field, the system displays 'Email is required'."

Use inversion to find unwanted behaviors: "What should the system
definitely NOT do?" This naturally surfaces the IF/THEN failure-handling
statements that are often more important than the happy path.

### Phase 5 — Resolve unknowns

For each unknown from the brief:
- Resolve it if the codebase or conversation answered it
- Ask the user if it requires a decision about intent
- Flag it for design-approach if it's a solution choice (which API,
  which technology, which deployment model)

Do not silently resolve unknowns. Do not make solution choices — those
belong in design-approach.

### Phase 6 — Assemble and confirm

Once all areas have been discussed, assemble the full
`behaviors.diff.md` and show it to the user:

> "Here's everything we discussed, assembled. Does the full picture
> hold together? Anything feel wrong now that you see it all at once?"

This is a final coherence check, not a repeat of the area-by-area
review. If something needs changing, fix it and confirm again.

After user approval, keep the approved behavior diff with the active
planning context that will be passed to `factory work create
--behaviors-file` after the plan is approved. Set legacy status to
`behaviors-defined` only when operating in a legacy fallback or recovery
path.

---

## Output format

```markdown
# Behaviors (diff)

Work Item: [work-item-id]
Brief: [one-line summary from the brief]

## Vocabulary

- **[Term]** — [definition in the user's words]
- **[Term]** — [definition]

## [Area 1]

WHEN [event or condition]
THE SYSTEM SHALL [observable behavior]
Example: [concrete instance]

IF [unwanted condition]
THEN THE SYSTEM SHALL [recovery behavior]

## [Area 2]

WHILE [state]
THE SYSTEM SHALL [observable behavior]

## Open questions

- [Decision deferred to design-approach]
```

---

## Rules

- **Interview, don't present.** Work through behaviors with the user
  in small pieces. Don't produce a complete document and ask for
  approval. This applies through the entire conversation — including
  review output, triggering mechanics, and final assembly. Don't
  start with small pieces and then dump the rest at the end.
- **One area at a time.** Don't overwhelm with the full picture until
  the pieces are agreed. Each area gets its own discussion turn.
- **Observable, not internal.** Every behavior describes something a
  user or external system can observe.
- **No implementation.** Do not specify technologies, libraries, or
  architecture. That belongs in design-approach.
- **Incremental.** The behaviors extend `documentation/behaviors.md`.
  Do not restate what already exists. Reference existing behaviors
  when new ones depend on them.
- **Don't over-specify implicit behavior.** Conventions are implicit.
  Only call them out when ambiguous or when the project departs from
  convention.
- **Flag what you added.** If you derive a behavior not in the brief,
  say so. Let the user confirm or reject.
- **Testable.** Each behavior should suggest how to verify it. If you
  can't imagine a test, the behavior is too vague. Each EARS statement
  in `documentation/behaviors.md` must have either a `Test:` reference
  naming the test that verifies it or an `Untestable:` marker with a
  one-line reason explaining why it genuinely cannot be tested.
- **One behavior per statement.** Do not combine multiple behaviors.
  Split them.
- **Vocabulary matters.** Use the terms the user uses. Pin them down
  early. New terms get recorded explicitly.
- **Concrete examples help.** When an EARS statement could be read
  multiple ways, add an example.
