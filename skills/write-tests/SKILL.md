---
name: write-tests
description: >
  Write tests for code changes or existing code. Read the code under test,
  choose test cases using equivalence partitioning and boundary analysis,
  structure tests clearly, and apply domain-specific patterns for areas
  like terminal UIs. Produces well-structured tests that verify behavior.
---

# Write tests

Write tests by reading the code under test, choosing cases that find bugs
efficiently, and structuring each test for clarity. Load testing expertise
before writing and apply domain-specific patterns when the code requires
them.

---

## How to run this skill

### Phase 1 — Read the inputs and load expertise

Read `references/tests.md` — the principles for writing tests.

Read the code that needs tests. Understand:
- The public interface — what callers can do and what they observe
- The branching logic — where the code makes decisions
- The dependencies — what external systems the code talks to
- The error paths — how the code handles failures

If the code includes a terminal UI, read `references/terminal-ui.md`
for TUI-specific testing patterns.

Identify the testing framework and conventions already used in the
project. Match them — don't introduce a new framework or style.

### Phase 2 — Choose test cases

Don't test exhaustively. Choose inputs that find the most bugs with the
fewest tests.

**Partition the input space.** Group inputs into categories that should
behave the same way. Test one representative from each category. If a
function accepts ages 0–120, you need a valid age, a negative age, and
an age above 120 — not 121 separate tests.

**Test boundary values.** Bugs cluster at edges. For a range of 1–100,
test 0, 1, 100, and 101. Pick boundary values from each partition
rather than values in the middle.

**Cover error paths.** For each way the code can fail — invalid input,
missing data, dependency errors — write a test that triggers that
failure and verifies the code handles it correctly.

**Identify independent dimensions.** If a function has four input modes
and three output formats, test each mode and each format separately,
then add one integration test that wires them together. A focused set
of tests gives you the same confidence as a full Cartesian product with
less maintenance cost.

**Skip trivial code.** Don't test getters, constants, simple delegation,
or framework guarantees. Focus on code with logic and branching.

### Phase 3 — Structure each test

**Name after the scenario.** A reader should understand what the test
verifies from the name alone.

```
# Vague
test_validation
test_process

# Clear
test_empty_email_shows_error
test_expired_token_returns_401
```

**Follow arrange-act-assert.** Set up preconditions, perform the action,
verify the outcome. When setup is long, extract a helper so the test
body stays focused on the action and assertion.

**One behavior per test.** If the name needs "and," split it. When a
multi-assertion test fails, you have to read the body to find the
problem. With one behavior per test, the name tells you.

**Test observable behavior.** Assert on what the public interface
returns or what side effects are observable. Don't assert on internal
state, call counts, or implementation details. If you refactor internals
without changing behavior, no test should break.

**Keep tests isolated.** Each test sets up its own state and cleans up
after itself. No test depends on another test running first. No test
depends on external state like clocks or network services unless
controlled.

### Phase 4 — Apply domain-specific patterns

#### Terminal UIs

When testing terminal UI code, apply the patterns from
`references/terminal-ui.md`:

- Render to an in-memory terminal buffer instead of a real terminal.
  Construct state, call the render function, and assert on the buffer
  contents.
- Assert that text appears somewhere on screen rather than at exact
  row/column coordinates. Content shifts when the terminal resizes.
- Test state transitions by rendering state A, mutating to state B,
  rendering again, and verifying the change is reflected. This catches
  stale rendering.
- Check animation state, not exact frames. Assert the animation
  indicator is present when active and absent when inactive.
- Extract `buffer_text`, `cell_at`, and `has_style` helpers into a
  shared test module once the first few TUI tests need them.

#### External dependencies

Prefer fakes over mocks. A fake that behaves like the real dependency
catches bugs that mocks miss; for example, an in-memory database can
catch malformed queries that a mock would accept.

Mock only at system boundaries such as external APIs, databases, and
network services. Don't mock your own code.

If many doubles are needed, the code probably has too many dependencies.
Consider restructuring the code before writing the test.

### Phase 5 — Validate

Run the tests. All new tests must pass.

If a test is flaky — passes sometimes, fails sometimes — fix or remove
it immediately. Flaky tests train developers to ignore failures.

Check that the tests actually exercise the code under test. A test that
passes because it asserts on a copy of the logic rather than calling
the real code gives false confidence.

---

## Rules

- **Read the expertise first.** Load `references/tests.md` before
  writing any tests. Apply its principles, not general assumptions.
- **Match existing conventions.** Use the project's testing framework,
  naming style, and file organization. Don't introduce new patterns.
- **Behavior, not implementation.** Every assertion verifies something
  observable through the public interface.
- **Efficient coverage.** Choose test cases that find distinct bugs.
  Redundant tests for equivalent inputs waste maintenance effort.
- **Don't test what's already tested.** Check existing tests before
  writing new ones. Avoid duplicating coverage.
- **Keep tests fast.** If a test is slow, the problem is usually the
  code's design, not the test. Restructure so logic is testable in
  isolation.
- **Failures should be obvious.** When a test fails, the cause should
  be clear from the test name and assertion. If you need a debugger,
  the test is too coarse.
