
# Define Behaviors

Interview the user. Write EARS statements that specify observable behaviors of the system. Design and implementation belong to later stages.

Your statements build on `documentation/behaviors.md` — an increment over what already exists, not a restatement — but the only file you write is the diff at `.fluent/drafts/<draft-id>/behaviors.diff.md`. If `documentation/behaviors.md` doesn't exist yet, this is the first behaviors set: still write `behaviors.diff.md`, where every statement is an addition.

## Read the inputs

Load these before starting the conversation:

- The confirmed brief at `.fluent/drafts/<draft-id>/brief.md`. The `<draft-id>` is set by `capture-brief`.
- Existing behaviors at `documentation/behaviors.md` (if it exists) — what the system already guarantees.
- Existing architecture at `documentation/architecture.md` (if it exists) — established naming conventions and system structure.
- `.fluent/expertise/decisions.md` (if it exists) — recorded project choices. New behaviors must not contradict them; surface any conflict for the user to resolve.
- The code the brief touches — enough to see how it behaves today where the new statements apply, and any existing behavior they'd modify.

Read `references/behaviors.md` for the EARS patterns and the qualities of a good statement.

If part of the brief is too vague to elaborate, ask the user before proceeding. Don't invent intent.

## Establish vocabulary

Pin down what things are called before writing any statement. The brief introduces terms loosely — this is where they get precise.

Where the brief is ambiguous or the existing system may already have a name for the concept, ask:

> "You called it a 'status feed' in the brief. Is that (a) the same as the existing `event-log` in `dashboard/`, (b) a new stream layered on top of it, or (c) a replacement?"

Prefer vocabulary already present in `documentation/behaviors.md` or `documentation/architecture.md`. When the brief introduces a term that isn't in either, note it explicitly in the diff's Vocabulary section so a future reader knows it isn't a synonym for something existing.

Keep this short — a few load-bearing terms, not a glossary. Move on when the core nouns and verbs are clear enough to write statements against.

## Map the space

Walk the three dimensions — actors, events, states — one at a time with the user:

> "The actors I see are the dashboard client and the cache. Is there (a) an admin surface too, (b) an external subscriber, or (c) just those two?"

> "The events I have are: a write invalidates a cache entry, and the dashboard subscribes to the feed. What am I missing?"

For small scoped work, walk only the dimensions that change.

Don't write EARS statements yet. Just agree on the map. Once the map is clear, group into areas — one area per cluster of related events. When the changes span several subsystems, group by subsystem instead.

## Work area by area

Handle one area per turn. For each area:

Propose the two or three core behaviors — the ones the brief clearly asks for — in EARS notation, so the user is reading the actual statement:

> "For the invalidation stream:
> WHEN a cache entry is invalidated,
> THE SYSTEM SHALL emit an `invalidated` event on the status feed
> carrying the entry key.
> Does that match what you had in mind?"

Then ask about the gaps — one question per turn. Inversion surfaces what the system should NOT do, which usually matters more than the happy path:

> "What should happen if the subscriber is disconnected when the event fires? (a) drop the event, (b) buffer up to N, (c) reject the write until reconnected."

Skip conventions the user expects by default — invalid input triggers an error message, slow work shows progress. Call a convention out only when the case is ambiguous or the project intentionally departs from it.

If you propose a behavior the brief didn't mention, flag it as derived so the user can accept or reject it.

When a statement could be read more than one way, pin it down with an `Example:` line giving a concrete instance.

If the user rejects a proposed statement, ask what's wrong before revising. Drop it if the user says it's out of scope. Don't re-propose the same behavior reworded.

Move to the next area only when the user has confirmed this one. Don't queue up remaining areas at the end.

## Resolve the brief's unknowns

Walk each unknown recorded in the brief:

- If the conversation or the codebase answered it, fold the answer into the relevant area's statements — or note it under Vocabulary if it resolved a naming question.
- If it needs a decision about what the system should do, ask the user now.
- If it's a solution choice — which library, which protocol, which storage — leave it for `design-approach` and record it under Open questions.

The brief may also record constraints and assumptions. Fold any that name observable behavior into the relevant area's statements; leave the rest for `design-approach`.

Don't silently resolve unknowns. Don't make solution choices here.

## Assemble and confirm

Once every area is agreed, write `behaviors.diff.md` to `.fluent/drafts/<draft-id>/behaviors.diff.md` and show it to the user:

> "Confirm the behaviors diff and move to approach? Reply **yes**, or name what to revise: (a) vocabulary, (b) a statement, (c) an unknown."

Check that terms are used consistently, no two statements contradict, and no two statements say the same thing in different words. If something needs changing, name which part — vocabulary, an area's statements, or an unresolved unknown — and re-enter that step. Don't re-run the whole area-by-area review.

Once the user confirms, stop. `design-approach` picks up next.

## Behaviors diff format

```markdown
# Behaviors (diff)

Draft id: [draft-id]
Brief: [one-line summary from the brief]

## Vocabulary

- **[Term]** — [definition in the user's words]

## [Area]

### B1

WHEN [event]
THE SYSTEM SHALL [observable behavior]
Test: [tests/status_feed.rs::emits_invalidated_event_on_write]
Example: [concrete instance, when the statement could be misread]
Derived: [what in the brief or codebase this follows from — present only when the brief didn't state it]
Modifies: [prior Area:B<N>] — [what changes]
Removes: [prior Area:B<N>] — [why the behavior no longer applies]

### B2

IF [condition]
THEN THE SYSTEM SHALL [recovery behavior]
Untestable: [one-line reason it can't be verified from a test]

## Open questions

- [Solution choice deferred to design-approach]
```

Omit sections with no content. Reference existing statements when a new one depends on them. When a statement changes existing behavior, add a `Modifies:` or `Removes:` marker naming the prior statement.

## Rules

- Label options as (a), (b), (c), or ask a yes/no with an obvious default. Avoid unlabeled "X or Y?" forms.
- Every new EARS statement carries either a `Test:` reference or an `Untestable:` marker with a one-line reason. The test usually doesn't exist yet — name the intended test path in the project's style (inspect nearby tests to match the naming convention); the writer creates it during execution. Use `Untestable:` only when the behavior genuinely can't be observed from a test.
- Number each area's statements as `### B1`, `### B2`, ... in reading order. IDs restart at `B1` in each area, so a reference like `B2` is always relative to its area.
