# How to specify behaviors

## Contents

- Why specify behaviors
- EARS patterns
- Properties of a good behavior statement
  - Says what, not how
  - Captures one behavior
  - Suggests a way to verify
  - Has one interpretation
- Coherence across behavior statements
  - Statements should use consistent vocabulary
  - Statements should not contradict each other
  - Statements should cover all intended behaviors
  - Statements should not repeat each other
- How to discover behaviors
  - Map actors, events, and states
  - Use inversion — ask what the system should NOT do
  - Cover error paths explicitly
- What not to specify
  - Solution choices
  - Non-behavioral goals

## Why specify behaviors

A behavior is something a system does that a user or external system can observe — an action it takes, a state it enters, a response it gives. A behavior statement records one such behavior.

When the code diverges from a behavior statement, either the code is wrong or the behavior statement is stale — but the gap becomes explicit rather than a matter of disagreement.

Behavior statements survive implementation changes. Rewrite the system, port it, or restructure it internally — because the observable behaviors don't change, the statements describing them stay valid. That durability is what makes behavior statements useful across the lifetime of the project.

A behavior statement is a long-term contract. Record only the behaviors you want to guarantee across future changes to the system. Write each behavior statement at the level of abstraction you're actually committing to. "A loading indicator shows during long operations" is a durable behavior statement; "the spinner is blue and pulses every 800ms" usually isn't — unless the specific look carries meaning.

## EARS patterns

EARS (Easy Approach to Requirements Syntax) is a notation for specifying behaviors precisely. Each behavior statement fits one of six patterns:

| Pattern | Template | Use when |
|---|---|---|
| Ubiquitous | THE SYSTEM SHALL [behavior] | Always true |
| Event-driven | WHEN [event], THE SYSTEM SHALL [behavior] | Triggered by an event |
| State-driven | WHILE [state], THE SYSTEM SHALL [behavior] | True during a condition |
| Unwanted | IF [condition], THEN THE SYSTEM SHALL [behavior] | Handling failures |
| Optional | WHERE [feature], THE SYSTEM SHALL [behavior] | Configurable |
| Complex | WHILE [state], WHEN [event], THE SYSTEM SHALL [behavior] | Compound |

The trigger word (WHEN, WHILE, IF, WHERE) tells the reader when the behavior applies. "THE SYSTEM SHALL" forces the statement to name what the system does — not how.

## Properties of a good behavior statement

### Says what, not how

A behavior statement describes what a user or external system sees, not what happens inside the code. "The system rejects a duplicate order" is observable. "The service returns a 409 with `{error: 'duplicate'}`" is observable at the API boundary. "The `OrderService::validate()` method returns `Err(Duplicate)`" isn't — that's implementation.

### Captures one behavior

Each behavior statement captures one behavior. If it has two triggers or two effects joined by "and," split it into two. "WHEN a user submits a valid form, the system stores the record and sends a confirmation email" is two behaviors — the storage and the email. Splitting them makes each verifiable on its own and lets the code implement one without breaking the other.

### Suggests a way to verify

Each behavior statement should suggest how to verify the behavior. If you can't imagine a test — an input, a check, an assertion — it is too vague to guarantee. "The system responds quickly" isn't testable; "the system responds within 200ms for a request under 1 MB" is. Verifiability keeps behavior statements from becoming aspirational.

### Has one interpretation

Each behavior statement should have one interpretation. Vague terms — "quickly," "sensible," "large," "user-friendly" — invite disagreement about whether the behavior holds. Replace them with concrete thresholds, or add a concrete example alongside it to pin down what it means.

## Coherence across behavior statements

### Statements should use consistent vocabulary

Use one term per concept, consistently across all behavior statements. Two names for the same thing — "user" here, "member" there — leaves the reader guessing whether they're the same. Match the project's existing vocabulary where possible; when you introduce a new term, note it explicitly so future readers know it wasn't a synonym.

### Statements should not contradict each other

No two statements should assert different things about the same situation. "WHEN a user submits a form with a missing email, the system displays 'Email is required'" and "WHEN a user submits a form with a missing email, the system silently accepts the submission" can't both be true. Contradictions surface as bugs later — either the code implements one and violates the other, or the code implements neither correctly.

### Statements should cover all intended behaviors

An intended behavior with no matching statement is a gap. Common examples: what happens on success is written down but what happens on failure isn't. Or a feature has some behaviors written down but not others.

### Statements should not repeat each other

Each behavior belongs in exactly one statement. When two statements say the same thing in different words, one becomes stale as the system evolves — and the reader has to work out whether they're still equivalent or now diverging.

## How to discover behaviors

### Map actors, events, and states

Before writing statements, sketch the territory. Actors are who or what interacts with the system (user, admin, external service, scheduler). Events are what triggers behavior (user actions, API calls, timeouts). States are ongoing conditions that matter (logged in, offline, rate-limited). Together they suggest what needs a statement — each combination of actor, event, and state is a potential behavior.

### Use inversion — ask what the system should NOT do

After sketching what the system should do, flip the question and ask what it should NOT do. "Should a user see another user's data? Should a submitted form submit again? Should a payment ever be double-charged?" The answers surface prohibitions and invariants — statements about what must never happen, easy to miss when the focus is on what should happen.

### Cover error paths explicitly

For each input, dependency, and state transition, enumerate the ways it can go wrong and write an IF/THEN statement for each. The payment gateway can time out, return malformed JSON, or return 500. The input can be missing, out of range, or wrong type. The happy path gets exercised constantly during development, so it tends to be spelled out; failure modes only fire under specific conditions and are more likely to ship without a statement.

## What not to specify

### Solution choices

A behavior statement describes what the system does, not which technologies it uses to do it. "The system uses PostgreSQL for storage." "The API follows REST conventions." "Authentication uses OAuth 2.0." These are solution choices, not behaviors — they belong in a design or approach document.

### Non-behavioral goals

A behavior statement describes what the system does, not the engineering work behind it. "Refactor the auth module for maintainability." "Achieve 80% test coverage." "Reduce technical debt." These are goals about the codebase, not user-observable behaviors — they belong in a project plan or roadmap.
