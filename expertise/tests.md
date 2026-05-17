# How to write tests

Principles for writing tests that give confidence without becoming a
maintenance burden.

## Why tests exist

Tests answer one question: does the system work? Every test decision
— what to test, how to test it, how many tests to write — should be
evaluated against this: does this test increase confidence that the
system works, proportional to its cost?

Tests also serve as living documentation. A well-written test shows
how the code is meant to be used and what it's expected to do. Unlike
comments or docs, tests stay in sync with the code because they break
when the code changes.

## Test behavior, not implementation

Tests should verify what the system does, not how it does it. A test
that breaks when you refactor without changing behavior is a tax, not
an asset.

**Behavioral test:** "when the user submits an empty form, an error
message appears." This survives any refactoring of the form
validation logic.

**Implementation test:** "the validateEmail function returns false
for empty strings." This breaks if you rename the function,
restructure the validation, or move the logic — even if the behavior
is unchanged.

Implementation tests have their place: complex algorithms, parsing
logic, mathematical computations where the implementation IS the
behavior. But they should be the exception, not the default.

The practical test: if you can refactor the code without changing
what users observe, and a test breaks, that test was testing
implementation.

## Testing levels

Different levels of tests catch different kinds of problems. No
single level is sufficient.

### Unit tests

Test individual components in isolation. Fast, numerous, good for
logic and edge cases. The foundation of any test suite.

Good for: business logic, data transformations, algorithms,
validation rules, pure functions.

Not good for: verifying that components work together, database
interactions, API contracts, user-visible behavior.

### Integration tests

Test that components work together correctly. Slower than unit
tests but catch a different class of bugs — the kind where each
piece works individually but they fail when connected.

Good for: database operations, API calls, file system interactions,
component interactions, configuration loading.

The sweet spot for most projects. Integration tests give the best
ratio of confidence to cost. They're slower than unit tests but
catch more real-world bugs.

### End-to-end tests

Test complete user journeys through the actual system. Maximum
confidence but slow, brittle, and expensive to maintain.

Good for: critical user paths (login, checkout, data export), smoke
tests that verify the system starts and responds.

Keep these few. They're notoriously flaky — failing for reasons
unrelated to code changes (network timeouts, rendering differences,
race conditions). Every flaky end-to-end test erodes trust in the
entire test suite.

### Contract tests

Verify that the interface between two systems matches what both
sides expect. Catch breaking changes between services without
running the full system.

Good for: API boundaries, service-to-service communication, any
place where one team's changes could break another team's code.

### Static analysis

Not tests in the traditional sense, but catches issues before any
test runs: type errors, unused variables, style violations, common
bug patterns. The cheapest form of quality checking.

## How many tests to write

Write lots of fast unit tests, some integration tests, and very few
end-to-end tests. This is the testing pyramid — more tests at the
bottom, fewer at the top.

The exact ratio depends on the project. A CLI tool needs mostly unit
tests. A payment gateway needs mostly integration tests. A website
needs more UI tests. Don't apply a standard pyramid to every project
— understand where your application's risks are and test those areas
most heavily.

Diminishing returns set in around 70% coverage. Don't chase 100% —
it leads to tests for trivial code and slows down refactoring. The
exception: small, reusable libraries where complete coverage is
manageable and valuable.

When a higher-level test catches a bug that no lower-level test
caught, write a lower-level test for it. Push test coverage down the
pyramid where tests are faster and more reliable.

## Test design techniques

### Equivalence partitioning

Group inputs into classes that should behave the same way. Test one
value from each class instead of every possible value. If a function
accepts ages 0-120, you don't need 121 tests — you need tests for a
valid age, a negative age, and an age above 120.

### Boundary value analysis

Bugs cluster at boundaries. If a function accepts 1-100, test at 0,
1, 2, 99, 100, and 101. The values just inside and outside each
boundary are where off-by-one errors live.

### Equivalence partitioning + boundary analysis together

Partition the input space, then test the boundaries of each
partition. This gives good coverage with minimal tests.

### Property-based testing

Instead of specifying exact inputs and outputs, specify properties
that should hold for any input. The testing framework generates
random inputs and checks the properties. Good for finding edge cases
you wouldn't think to test manually.

Example property: "sorting a list and then sorting it again produces
the same result as sorting it once." The framework tries hundreds of
random lists to verify this.

## Test quality

### Naming

The test name should describe the behavior being verified. A reader
should understand what the test checks from the name alone.

```
# Vague
test_validation
test_process

# Clear
test_empty_email_shows_error
test_expired_token_returns_401
test_creates_run_directory_with_status_file
```

### Structure

Use arrange-act-assert:

1. **Arrange** — set up preconditions
2. **Act** — perform the action
3. **Assert** — verify the outcome

Keep each section clear. When arrange is longer than act + assert,
the test may be testing too much, or the setup should be extracted
into a helper.

### One behavior per test

Each test verifies one thing. When a test with multiple assertions
fails, you need to read the test body to know what broke. With one
behavior per test, the name tells you.

If the test name needs "and," split it.

### Self-contained and isolated

Tests should not depend on each other or on shared mutable state.
Running tests in any order, or running a single test alone, should
produce the same result.

Each test sets up its own state and cleans up after itself. Use
temporary directories, fresh instances, and setup/teardown
functions. Don't rely on state left by a previous test.

### Deterministic

A test that sometimes passes and sometimes fails (without code
changes) is worse than no test. Flaky tests destroy confidence in
the entire suite. Developers learn to ignore failures, and real
regressions slip through.

Common causes of flakiness: timing dependencies, shared state
between tests, reliance on external services, non-deterministic
ordering, race conditions. Fix or remove flaky tests immediately.

### Tests are code

Apply the same quality standards to test code as to production code.
Extract common setup into helpers. Name things clearly. Don't
copy-paste tests — if multiple tests share setup, factor it out.
Poorly maintained tests accumulate debt faster than production code.

## Test doubles

Test doubles (mocks, stubs, fakes, spies) replace real dependencies
in tests. Use them deliberately.

**Mock:** records what was called and with what arguments. Verify
that the right calls were made.

**Stub:** returns predetermined responses. Control what the
dependency provides to the code under test.

**Fake:** a lightweight working implementation. An in-memory
database, a local file system, a simple HTTP server.

**When to use doubles:** for external dependencies that are slow
(network, database), non-deterministic (time, randomness), or
expensive (payment APIs, cloud services).

**When not to use doubles:** for internal modules that are fast and
deterministic. Excessive mocking means you're testing that your mocks
work, not that your code works. If a test needs many mocks, the code
under test may have too many dependencies.

## What not to test

**Trivial code.** A function that returns a constant or a simple
getter doesn't need a test. Focus on code with logic, branching,
and integration points.

**The framework.** If the language guarantees that assignment works,
don't test assignment. Test what your code does, not what the
runtime does.

**Private internals.** If you need to test a private function
directly, it might belong in its own module with a public interface.
Or the behavior it implements should be tested through the public
API.

**Configuration format.** Don't test that YAML parses correctly
unless parsing is your code's job. Test the behavior the
configuration enables.

## Test the actual code

Tests must exercise the real implementation, not a copy of it.

**The replication anti-pattern.** When an agent can't easily test
internal logic, it copies that logic into the test and verifies the
copy instead of the original. The test passes, but it's testing
itself — if the real code changes, the test still passes because it's
running its own duplicate. This gives false confidence.

If you can't test a piece of code through its public interface,
consider these options in order:

1. **Restructure the code.** Can the logic be extracted into a
   module with its own interface? Often the reason something is hard
   to test is that it's buried in a function that does too much.

2. **Relax visibility constraints.** It's acceptable to make
   something more visible (package-private, internal, pub(crate))
   specifically to make it testable. A slightly wider interface that
   enables real testing is better than a perfectly encapsulated
   module you can't verify. This is a justified trade-off, not a
   design failure.

3. **Test through a higher-level interface.** If the behavior is
   observable at a higher level, test it there. The internal function
   gets coverage indirectly.

4. **As a last resort, duplicate carefully.** If none of the above
   work and the logic is critical, duplicating it in the test can be
   justified — but document why, and recognize that the test won't
   catch changes to the original. This should be rare and flagged for
   future improvement.

The priority is: test the real code. Adjust the code's design to be
testable rather than adjusting the test to avoid the code.

## Anti-patterns

**Testing implementation details.** Tests coupled to code structure
break on refactoring. They cost maintenance without catching real
bugs.

**Excessive mocking.** Every mock is an assumption about how the
dependency behaves. Too many assumptions mean the test verifies a
fantasy, not reality.

**Flaky tests kept in the suite.** A flaky test teaches everyone
to ignore test failures. Fix it or remove it.

**Large test functions.** A test with 50 lines of setup is testing
too much. Break it into focused tests with shared helpers.

**100% coverage as a goal.** Coverage measures which lines executed,
not whether the tests verify anything useful. A test that calls code
without asserting outcomes adds coverage without adding confidence.

**Not converting production bugs to tests.** Every production bug
is a testing gap. When you fix a bug, write a test that would have
caught it. This prevents regression and documents the issue.

**Snapshot tests as the only test.** Snapshots detect changes but
don't verify correctness. A changed snapshot tells you something
changed — not whether the change is right. Use behavioral assertions
alongside snapshots.

**Testing the same thing at multiple levels.** If a unit test covers
a case, don't repeat it in an integration test. Each test level
should catch a different class of bugs.

**Slow test suites.** If tests take too long, developers stop
running them. Keep the fast suite under 30 seconds. Isolate slow
tests into a separate suite for CI.
