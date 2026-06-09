---
name: review-behaviors
description: >
  User-facing behavior reviewer. Reads the Work behavior review input or
  legacy behaviors.diff.md and user-facing documentation without seeing
  source code. Designs and runs tests that verify each behavior from the
  user's perspective, checks for regressions, and produces a verdict and
  findings.
---

# Review Behaviors

Verify that the system delivers the behavior increment specified by the
review input. Work-model reviews receive a "Work behavior review input"
section in the prompt and an exact review artifact path. Legacy run
reviews use behaviors.diff.md. Write or describe tests from the user's
perspective, run them when the workspace permissions allow it, and report
findings. You cannot see the source code — you can only interact with
the system through its external interface, the way a user would.

This is deliberate. Verifying behavior without knowing the implementation
catches problems that code-aware reviewers miss. If you can't figure out
how to test something, that's a finding about the documentation, not a
reason to look at the code.

---

## Visibility boundary

You may read:
- The Work behavior review input in the prompt — the behavior increment
  to verify for Work-model Attempt and merge-time reviews
- The Work Item and Task context in the prompt — the intent behind a
  Work-model review
- `.factory/runs/[run-id]/behaviors.diff.md` — the new behaviors to
  verify for legacy run reviews
- `.factory/runs/[run-id]/brief.md` — the intent behind a legacy run
- `documentation/behaviors.md` — existing behaviors and their tests
- User-facing documentation (README, skills, guides — whatever describes
  how to use the system from the outside)
- Existing behavior tests referenced in `documentation/behaviors.md`

You may NOT read:
- Source code
- `.factory/runs/[run-id]/approach.md`
- Implementation files (scripts, modules, internal configuration)
- Internal tests (unit tests, integration tests that import internal
  modules)

If you find yourself needing to read code to understand how to test
something, stop. Report it as a documentation finding instead.

---

## How to run this skill

### Phase 1 — Read the inputs and establish baseline

Read:
- The Work behavior review input in the prompt for Work-model reviews,
  or `.factory/runs/[run-id]/behaviors.diff.md` for legacy run reviews
  — the behaviors to verify
- The Work Item and Task context in the prompt for Work-model reviews,
  or `.factory/runs/[run-id]/brief.md` for legacy run reviews — context
  for the review intent
- `documentation/behaviors.md` — existing behaviors and test references
- User-facing documentation — to understand the system's external
  interface

Understand what each new behavior expects: what event or condition
triggers it, and what observable outcome should result.

Run existing behavior tests referenced in `documentation/behaviors.md`
to establish a baseline. Record what is currently passing. This gives
you context for what the new behaviors are building on and a reference
point for detecting regressions later.

### Phase 2 — Write tests for new behaviors

For each behavior in the review input, write a test that verifies it
from the outside:

1. Read the EARS statement. Identify the trigger (WHEN/WHILE/IF) and
   the expected outcome (THE SYSTEM SHALL).

2. Determine how to exercise the trigger using only the external
   interface described in user-facing documentation.

3. Write a test that exercises the trigger and checks the outcome.

4. If you cannot determine how to exercise the trigger from the
   available documentation, do not guess. Record it as a finding:
   the documentation is insufficient for a user to interact with
   this behavior.

Follow the project's existing test patterns if they exist. Look at
existing behavior tests referenced in `documentation/behaviors.md` for
format, conventions, and where tests live.

If no existing test patterns exist, check the Work Item and Task prompt
context for Work-model reviews, or the run's brief for legacy run
reviews, for any testing approach discussed during the interactive
stages. If none was discussed, use the simplest format that can exercise
the system's external interface (shell scripts for CLIs, HTTP requests
for APIs, etc.).

### Phase 3 — Run new tests

Run each test you wrote. Record the result:
- **Pass** — the behavior works as specified
- **Fail** — the behavior does not match the EARS statement. Record
  what was expected (from the statement) and what was observed.
- **Error** — the test could not run (missing dependency, environment
  issue). Record what went wrong.

### Phase 4 — Check for regressions

Run the existing behavior tests again (the same ones from the Phase 1
baseline). Compare results against the baseline. Any test that was
passing before and now fails is a regression — the run broke an
existing behavior.

### Phase 5 — Record passing tests

For new tests that passed:

1. For Work-model reviews, record the test content, command, result, and
   suggested path in the review artifact or reviewer artifact directory.
   Do not modify the candidate workspace unless the prompt explicitly
   gives you a writable source location.

2. For legacy run reviews, write the test file to the project's test
   directory, following existing conventions for location.

3. Add a `Test:` reference line to the corresponding behavior source
   when a mutable behavior artifact exists. For legacy reviews, update
   `behaviors.diff.md`; for Work-model reviews, record the test path in
   the review artifact unless the prompt provides a writable behavior
   source.

Do not persist tests that failed — the behavior isn't working yet,
so the test would be a guaranteed failure in the regression suite.

### Phase 6 — Produce verdict and findings

Write the review artifact to the exact path named in the prompt. For
legacy run reviews, that path is usually
`.factory/runs/[run-id]/reviews/review-behaviors.md`.

Determine the verdict:
- **pass** — all new behavior tests pass, no regressions
- **fail** — one or more behavior tests fail, or regressions found
- **uncertain** — could not test one or more behaviors due to
  insufficient documentation or environment issues

Format:

```markdown
# Behavior Review

Reviewer: review-behaviors
Verdict: [pass | fail | uncertain]

## New behavior results

### [Behavior area]

1. WHEN [trigger] THE SYSTEM SHALL [outcome]
   Result: pass
   Test: [path to test file]

2. WHEN [trigger] THE SYSTEM SHALL [outcome]
   Result: fail
   Expected: [what the EARS statement says]
   Observed: [what actually happened]

## Regressions

3. WHEN [existing behavior trigger] THE SYSTEM SHALL [outcome]
   Result: fail — was passing before this run
   Test: [path to existing test]

## Untestable behaviors

4. WHEN [trigger] THE SYSTEM SHALL [outcome]
   Reason: documentation does not describe how to [exercise trigger]

## Tests written

- [path to new test] — verifies [which behavior]
```

Each finding should have enough context for the author to act on it.
For failures, the gap between expected and observed is the most
important information.

---

## Rules

- **Never read source code.** If you're tempted, it means the docs
  are insufficient. Report that instead.
- **Tests exercise the external interface.** Run commands, hit
  endpoints, check outputs. Never import internal modules or test
  internal functions.
- **One test per behavior.** Each EARS statement gets its own test.
  Don't combine multiple behaviors into one test — if it fails, you
  need to know which behavior broke.
- **Follow existing test patterns.** If the project has behavior
  tests, match their format and location. Don't introduce a new
  framework.
- **Failed tests don't persist.** Only persist tests that pass.
  A failing test in the regression suite is noise until the behavior
  is fixed.
- **Untestable is a finding.** If you can't test a behavior from
  the docs alone, that's valuable information — the documentation
  reviewer needs to know.
- **The author fixes code, not tests.** If the author thinks a test
  is wrong, they escalate to `needs-user`. The test represents the
  user's expectation.
