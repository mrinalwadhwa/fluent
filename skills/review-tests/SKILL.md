---

## Prior reviews of this candidate

When the inputs to your review Task include a previous review of this
candidate by your role, treat it as another reviewer's findings, not
as your past self. Read it first.

For each finding in that previous review:
- Verify against the current candidate state whether the writer
  addressed the concern.
- If addressed, note it in your "Prior concerns addressed" section.
- If not addressed, carry it forward into your current findings.

Then evaluate the current candidate independently and add any new
findings. The writer may have addressed prior concerns while
introducing new ones — both pieces of information matter.

Use the `Progress:` field to summarize whether you observed any
movement on prior concerns: `yes`, `no`, `partial`, or `first-pass`
(when no prior review exists). This is independent of `Verdict:` — a
failing `Verdict:` can co-occur with `Progress: yes` when prior
concerns were addressed but new ones emerged.

---
name: review-tests
description: >
  Code-aware test reviewer. Reads test files and the code they test,
  checks test quality against testing principles. Evaluates whether
  tests verify behavior, are well-structured, and provide confidence
  proportional to their cost. Produces a verdict and findings.
---

# Review tests

Review test files by reading them alongside the code they test and
the testing expertise. Check whether tests verify behavior (not
implementation), are well-structured, and provide meaningful
confidence. Produce findings the author can act on.

---

## Build outputs and warm cache

Factory pre-populates your artifact directory with copies of the writer's
build outputs for warm-start incremental builds. Point your toolchain
at this directory for incremental builds; reading binaries the writer
built directly from the candidate workspace is also fine.

---

## How to run this skill

### Phase 1 — Read the inputs and load expertise

Read `references/tests.md` — the principles for writing tests.
If the code under review includes a terminal UI, also read
`references/terminal-ui.md` for TUI-specific testing patterns.

Check how the review was triggered:

**Run-scoped (default):** Use the git diff to identify which test
files changed or which code changes should have corresponding tests.
Review those tests and check for missing test coverage on changed
code.

**Full-codebase:** Review all test files.

### Phase 2 — Check test quality

For each test file in scope, evaluate against the testing expertise:

**Behavior vs implementation:**
- Do tests verify observable behavior or internal implementation?
- Would the tests break if the code were refactored without
  changing behavior?
- Are tests coupled to function names, data structures, or call
  sequences that are implementation details?

**Structure:**
- Does each test follow arrange-act-assert?
- Is each test focused on one behavior?
- Are test names descriptive of the behavior being verified?
- Is setup reasonable in length — not longer than the test itself?

**Isolation:**
- Can tests run independently and in any order?
- Do tests clean up after themselves?
- Is there shared mutable state between tests?

**Determinism:**
- Are there timing dependencies, race conditions, or reliance on
  external services that could cause flakiness?
- Are random values seeded or controlled?

**Test doubles:**
- Are mocks and stubs used for external dependencies only?
- Is there excessive mocking of internal modules?
- Do mocks accurately represent the real dependency's behavior?

### Phase 3 — Check test coverage

For code changes in the run:

- Do new behaviors have corresponding tests?
- Are edge cases and error paths covered?
- Is testing effort focused on the right areas — critical logic
  over trivial code?
- Are there behaviors in `documentation/behaviors.md` that lack
  test references?

For full-codebase reviews:

- Which significant components have no test coverage?
- Are tests distributed appropriately across levels (unit,
  integration, behavioral)?

### Phase 4 — Check test design

- **Equivalence partitioning:** Are input spaces partitioned
  sensibly, or is there redundant testing of equivalent values?
- **Boundary values:** Are boundary conditions tested — the edges
  where off-by-one errors live?
- **Error paths:** Are failure cases tested, not just happy paths?
- **Test naming:** Can you understand what each test verifies from
  its name alone?

### Phase 5 — Check test maintenance

- **Duplication:** Are test helpers extracted, or is setup logic
  copy-pasted across test files?
- **Readability:** Can a new developer understand each test without
  reading the implementation?
- **Size:** Are individual test functions focused and short, or
  bloated with setup?

### Phase 6 — Produce verdict and findings

Write the review artifact to the exact path named in the prompt.

Determine the verdict:
- **pass** — no findings that warrant changes
- **fail** — findings the author should address before completion
- **uncertain** — findings that need the user's judgment

Format:

```markdown
# Test review

Reviewer: review-tests
Verdict: [pass | fail | uncertain]
Progress: [yes | no | partial | first-pass]

## Findings

### Behavior vs implementation

1. [test-file:test-name] — tests implementation detail [what],
   would break on refactoring without behavior change

### Structure

2. [test-file:test-name] — [issue: vague name, multiple behaviors,
   excessive setup]

### Coverage gaps

3. [code-file or behavior] — [what's untested and why it matters]

### Test design

4. [test-file] — [missing boundary tests, redundant equivalent
   tests, untested error paths]

### Maintenance

5. [test-file] — [duplicated helpers, copy-pasted setup]
```

---

## Rules

- **Read the expertise.** Check against `references/tests.md`, not
  your own assumptions about testing.
- **Findings, not rewrites.** Report what's wrong and where. The
  author determines the fix.
- **Severity matters.** A test that's flaky in CI is more urgent
  than a test with a vague name. Lead with findings that affect
  confidence.
- **Don't demand coverage for trivial code.** Getters, constants,
  and simple delegation don't need tests. Focus coverage findings
  on code with logic and branching.
- **Context matters.** A shell script test suite has different
  expectations than a React component test suite. Apply testing
  principles proportionally to the project type.
- **Tests that test the wrong thing are worse than no tests.**
  They give false confidence. Report tests that pass but don't
  verify anything meaningful.
