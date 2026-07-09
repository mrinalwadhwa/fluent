# How to write tests

## Contents

- Why tests exist
- Properties of good tests
  - Test observable behavior, not internal structure
  - Write tests that are isolated and deterministic
  - Keep tests fast and automated
  - Make tests easy to write and easy to read
  - Write tests so that when they fail, the cause is obvious
  - When production breaks despite passing tests, write a test that would have caught it
- How to choose test cases
- How to structure a test
- Testing levels
- Test doubles
- Test the actual code
- What not to test
- How to run tests
  - Always save test output to a durable file
  - Pick a runner that gives per-test isolation and parallel execution
  - Run all suites after cross-system changes
  - Where to find failure logs
  - When tests fail for an unclear reason, instrument before guessing
- Related guides
- Patterns

## Why tests exist

Tests build confidence in code. Each one encodes a scenario and an expected outcome. Every passing test ratchets up confidence: one more set of inputs that work as expected.

Once you have a repeatable test for a scenario, you can safely refactor the code that implements it. The test tells you immediately whether it still works. If your suite of tests covers enough ground, you can ship frequently and with confidence.

Writing a test makes you think about the interface. If the test is hard to write, the interface may need work.

Tests capture your thinking at the time of writing: what you expected for a given scenario. A teammate reading the code months later can look at the tests to understand intended behavior. But tests only cover what you thought to cover. Production will have scenarios you didn't think of.

## Properties of good tests

### Test observable behavior, not internal structure

Tests should be sensitive to changes in observable behavior and insensitive to changes in internal structure. A good test verifies behavior through the test subject's public interface and ignores the implementation behind it.

If you reorganize internals without changing the public interface or its outputs, no test should break. Rename a private method, extract a function, change a data structure — none of these should cause a test failure.

Common causes of structure-coupling: testing private methods directly, asserting on call counts, or mocking parts of your own code instead of mocking only external dependencies like databases or APIs.

The exception is when the goal is to implement a specific algorithm — a parser, a sort, a hash function. The algorithm is the specified behavior, and testing it directly makes sense.

### Write tests that are isolated and deterministic

Each test should set up its own state, run independently, and produce the same result every time. If a test passes when run alone but fails when run after another test, it depends on shared state it shouldn't. If a test passes on Monday and fails on Tuesday with no code changes, it depends on something outside its control: a clock, a random seed, a network service, a database that wasn't cleaned up.

Set up state in the test (or a setup hook); tear it down in a teardown hook so cleanup runs even when the test fails partway through. A test that leaves state behind contaminates every test that runs after it — turning a single bug into a cascade of false failures.

Flaky tests are worse than missing tests. A flaky test trains you to ignore failures, and real regressions slip through alongside the noise. Fix or delete flaky tests immediately.

Isolated tests compose naturally. If you have four ways to compute interest and five ways to report it, you don't need twenty tests. Test the four computations, test the five reports, add one test that wires them together. Nine tests give you the same confidence as twenty, and they're faster and easier to maintain. This works when the dimensions are genuinely independent. When they're not, test the combinations that matter.

### Keep tests fast and automated

Slow tests don't get run. If the suite takes long enough that you hesitate to run it, you'll start batching changes, and when something fails you won't know which change caused it.

Every test should run without human intervention. No manual setup, no visual inspection of output, no "check that the UI looks right." If a test requires a person to judge whether it passed, it won't get run often enough to catch anything.

When a test is slow, the problem is usually the design of the code under test, not the test itself. Code that requires a database, a network call, or a heavy setup to test a piece of logic can often be restructured so the logic is testable in isolation.

### Make tests easy to write and easy to read

If a test is hard to write, the code is probably hard to use. Simplify the interface.

Tests are read more than they're written. A reader should understand what a test checks from the test itself, without chasing through shared helpers or abstract base classes. Favor clarity in each test over eliminating duplication across tests. Some repetition between tests is fine if it keeps each one self-explanatory.

### Write tests so that when they fail, the cause is obvious

If you need a debugger to figure out why a test broke, the test is too coarse. Smaller tests that exercise less code point directly to the problem. When multiple tests fail at once, start with the leaf-level failures. They're usually the root cause.

### When production breaks despite passing tests, write a test that would have caught it

A passing suite should mean something. If all tests are green but production breaks routinely, the suite is testing the wrong things or not enough of the right things. You can't replicate every production condition in a test, but when you find a gap between test results and production behavior, close it.

Every production bug becomes a regression test before the fix lands. Write the test, watch it fail on the broken code, apply the fix, watch it pass. The bug now has a guard against silent reappearance.

## How to choose test cases

You can't test every possible input. The goal is to pick inputs that find the most bugs with the fewest tests.

Group inputs into categories that should behave the same way, and test one value from each category. If a function accepts ages 0-120, you don't need 121 tests. You need a valid age, a negative age, and an age above 120. If it handles the valid case correctly for 25, it will handle it correctly for 47.

Bugs cluster at boundaries. If a range is 1-100, test 0, 1, 100, and 101. The values just inside and just outside each boundary are where off-by-one errors live. Combine this with the categories: pick the boundary values from each group rather than values in the middle.

Cover error paths. For each way the code can fail — invalid input, missing data, a dependency that errors or times out — write a test that triggers the failure and verifies the handling. The happy path gets exercised constantly during development; error paths only fire under specific conditions, so they're more likely to ship broken.

For some code, specifying exact inputs and outputs is less useful than specifying properties that should hold for any input. "Sorting a list twice produces the same result as sorting it once." "Encoding then decoding returns the original." Property-based testing frameworks generate random inputs and check these properties, finding edge cases you wouldn't think to write by hand.

## How to structure a test

Name the test after the scenario it verifies. A reader should understand what the test checks from the name alone.

```
# Vague
test_validation
test_process

# Clear
test_empty_email_shows_error
test_expired_token_returns_401
```

Follow arrange-act-assert: set up the preconditions, perform the action, verify the outcome. When setup gets long, extract it into a helper so the test body stays focused on the action and assertion. When tests build similar objects with small variations, use a builder or fluent: it returns a fully-formed object from sensible defaults, and each test overrides only the fields it cares about.

Each test should verify one behavior. If the name needs "and," split it into two tests. When a multi-assertion test fails, you have to read the body to figure out what broke. With one behavior per test, the name tells you.

## Testing levels

- **Unit tests** test a single component in isolation. Fast, good for logic, edge cases, algorithms. The bulk of most test suites.
- **Integration tests** test that components work together. Slower, but they catch bugs that unit tests can't, the kind where each piece works alone but they fail when connected. Often the best confidence-to-cost ratio.
- **End-to-end tests** test complete user journeys through the real system. High confidence, but they're slower and more brittle than other levels. Failures often come from environment issues rather than code bugs.
- **Contract tests** verify that two systems agree on their shared interface. Useful at API boundaries where one team's changes could break another's.

No single level is sufficient. A project that only has unit tests won't catch integration bugs. A project that only has end-to-end tests will have a slow, flaky suite. Match the level to what you're verifying.

## Test doubles

Test doubles replace real dependencies in tests. The main types:

- **Fake**: actually works. You can write to it, query it, and get realistic responses. An in-memory database that stores and retrieves data. A local HTTP server that handles requests.
- **Stub**: you tell it what to return and it doesn't do anything else. A function that always returns the same user object regardless of what you ask for.
- **Mock**: you tell it what calls to expect, then check that those calls happened. An object that lets you assert "send_email was called with this address and this body."

Prefer real implementations when they're fast and deterministic. When they're not, prefer fakes. Fakes behave like the real thing and catch bugs that stubs and mocks miss because they execute actual logic. A test using an in-memory database will catch a malformed query. A test using a mock won't.

Mocks verify interactions, not outcomes. That makes them sensitive to how code is structured rather than what it does. If you refactor the internals, mock-based tests break even when behavior is unchanged. Reserve mocks for cases where the interaction itself is the behavior you care about: verifying that an email was sent, that a payment API was called with the right amount.

If a test needs many doubles, the code under test probably has too many dependencies.

A double gets called with whatever inputs the production code chooses to send — including invocation shapes the test author didn't anticipate. If your double branches on input shape (PWD, args, env vars, stdin), guard the branches early and exit cleanly when the shape doesn't match a known case. Otherwise an unexpected invocation falls through to whatever catch-all branch you wrote and produces side effects in the wrong context. The resulting bug surfaces two layers away from the double and is painful to trace.

## Test the actual code

Tests must exercise the code you're trying to verify. When code is hard to reach through its public interface, it's tempting to copy the logic into the test and verify the copy instead. The test passes, but it's testing itself. If the real code changes, the test still passes because it's running its own duplicate.

If you can't test a piece of code through its public interface:

1. **Restructure the code.** Extract the logic into a module with its own interface. Often the reason something is hard to test is that it's buried in a function that does too much.
2. **Relax visibility.** Making something package-private to enable testing is a justified tradeoff.
3. **Test through a higher-level interface.** If the behavior is observable at a higher level, test it there.
4. **Duplicate as a last resort.** If nothing else works and the logic is critical, duplicating it in the test can be justified. Document why, and recognize it won't catch changes to the original.

Adjust the code to be testable rather than adjusting the test to avoid the code.

## What not to test

- Trivial code. A function that returns a constant or a simple getter doesn't need a test.
- Framework guarantees. If the language guarantees that assignment works, don't test assignment.
- Configuration format. Don't test that YAML parses correctly unless parsing is your code's job.
- Private internals. If you need to test a private function directly, it probably belongs in its own module with a public interface.

## How to run tests

### Always save test output to a durable file

Redirect every test run's stdout and stderr to a file before inspecting. Don't pipe straight to the terminal: when one case fails inside a long chain, the failing case's full output may scroll past or be truncated by a filter, and reproducing it requires a slow rerun.

Pattern:

```bash
<test-command> > /tmp/test.out 2>&1
```

Then grep, less, or open the saved file. Subsequent debugging, post-mortem analysis, and references in summaries cite the saved file path. The file is your record of "what the run showed," without needing the run again.

When sharing a result in a code review or task handoff, reference the saved path so the reader can reopen the same evidence without rerunning.

### Pick a runner that gives per-test isolation and parallel execution

Use a runner with per-test isolation and parallel execution. Isolation runs each test in its own process so global state — env vars, filesystem, in-process singletons — can't leak between them. Parallelism distributes the suite across cores so it stays fast enough to run often.

For Rust, `cargo nextest run` does both — it spawns each test in its own process and parallelizes aggressively. Equivalent runners exist for other ecosystems (pytest-xdist for Python, jest with worker processes for JavaScript). Look for a "slow test" indicator in the runner's output — those are the wall-clock floor and worth tightening.

Run tests in parallel by default. The runner picks a sensible thread count automatically; serializing (e.g., `--test-threads=1`, `--runInBand`) trades real time for an isolation property the tests should provide themselves. If a test fails under parallel execution because it shares global state, fix the test — or scope its state per-test — rather than serializing the suite.

### Run all suites after cross-system changes

When a change touches data flow across subsystems — schemas shared between layers, generated output consumed by another component, types or paths multiple components depend on — run the full test suite, not just the suite closest to the changed file. Cross-system tests fail in indirect ways: a single suite stays green while a sibling suite accumulates debt that only surfaces when something else triggers a full run. The convenience of single-suite runs masks this; pay the wall-clock cost when the change shape warrants it.

### Where to find failure logs

Most test runners write per-case stdout and stderr to a known location after each run (check your runner's docs for "test output" or "log directory"). After a failure, open the log directly instead of rerunning the whole suite. If your runner doesn't capture per-case output by default, configure it to — re-running just to read what the test printed wastes wall-clock time and slows diagnosis.

### When tests fail for an unclear reason, instrument before guessing

If you can't tell from the failure message why the test broke — a side effect appears in the wrong place, output differs subtly, a value is wrong — add one or two targeted print or log statements to the suspect code path (including test doubles). Re-run once and inspect the new evidence. This is cheaper than iterating on hypothetical fixes. Remove the instrumentation once you've understood the cause.

## Related guides

Topic-scoped guides for specific areas. Read what applies to the code you're writing or reviewing.

- [terminal-ui](terminal-ui.md) — read when the code you're writing or reviewing is a terminal UI.

## Patterns

Reusable testing patterns live in `tests/patterns/` as individual files. Index each one below with a single-line load trigger so an agent reads a pattern only when its trigger applies. Add a `tests/<sub-topic>/` directory and a sub-topic file (with its own `## Patterns`) when a narrower theme accumulates enough material to warrant scoping.

- [asserting-on-generated-output](tests/patterns/asserting-on-generated-output.md) — read when asserting on output from template rendering, codegen, prompt generation, or any data-to-text transformation.
- [comparing-structured-data](tests/patterns/comparing-structured-data.md) — read when asserting on the content of a JSON/YAML/TOML file or any structured-data format that has multiple valid serializations.
