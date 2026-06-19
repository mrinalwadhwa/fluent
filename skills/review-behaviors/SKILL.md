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
name: review-behaviors
description: >
  Behaviors completeness reviewer. Reads tester-results.json and
  the behaviors diff to verify every new or changed EARS statement has a
  Test: or Untestable: marker and every Test: reference passed. Produces
  a verdict and findings.
---

# Review Behaviors (completeness)

Verify that the candidate's behavior increment is complete: every EARS
statement has test coverage or an explicit untestable marker, and every
referenced test passed. You do not write or run tests — the
Tester Task handles execution and produces
`tester-results.json` before you run.

---

## How to run this skill

### Phase 1 — Read inputs

Read:
- `tester-results.json` from the artifact path named in the
  prompt. This contains per-behavior statuses produced by the
  Tester Task.
- The Work behavior review input in the prompt — the behavior increment
  to verify.
- `documentation/behaviors.md` — existing behaviors and their markers.

### Phase 2 — Verify and produce verdict

For each new or changed EARS statement in the behavior increment:

1. Verify it has either a `Test:` reference or an `Untestable:` marker.
   Missing either → finding.

2. If it has a `Test:` reference, find the matching entry in the
   `tests` array of `tester-results.json`. Verify `status` is `pass`.
   If `fail`, include the `failure_excerpt` in your finding. If the
   test is not present in the array, the reference did not resolve to
   a real test — finding.

3. If it has an `Untestable:` marker, verify the rationale is
   reasonable. The Tester does not produce per-behavior untestable
   status — untestable markers are a documentation concern.

If the JSON `error` field is non-null:
- Produce a `fail` verdict naming the error `kind` and `message`.
- Do not flag individual behaviors — the test infrastructure itself
  failed.
- Verdict: fail.

Produce `review.md` with:

```markdown
# Behavior Review

Reviewer: review-behaviors
Verdict: [pass | fail | uncertain]
Progress: [yes | no | partial | first-pass]

## Findings

[List of findings, if any]
```

---

## Rules

- **Do not write or run tests.** The Tester Task handles
  execution. You verify completeness from its output.
- **Do not read source code.** If documentation is insufficient to
  understand a behavior, report that as a finding.
- **Every new EARS statement needs coverage.** Either a `Test:` reference
  that passed or an `Untestable:` marker with a reason.
- **command_failure is a single finding.** When the test infrastructure
  failed, do not flag individual behaviors.
