---
name: review-behaviors
description: >
  Behaviors completeness reviewer. Reads tester-results.json and the
  behaviors increment to verify every new or changed EARS statement has
  a Test: or Untestable: marker and every Test: reference passed.
  Produces a verdict and findings.
---

# Review Behaviors (completeness)

Verify that the candidate's behavior increment is complete: every EARS statement has test coverage or an explicit untestable marker, and every referenced test passed. You do not write or run tests — the Tester Task handles execution and produces `tester-results.json` before you run.

---

## How to run this skill

### Phase 1 — Read inputs

Read:

- `behaviors.md` (path in the user prompt's Phase 1) — the behavior increment to verify.
- `tester-results.json` (path in the user prompt's Phase 2) — per-test statuses produced by the Tester Task.

### Phase 2 — Verify

For each new or changed EARS statement in the behavior increment:

1. Verify it has either a `Test:` reference or an `Untestable:` marker. Missing either → finding.

2. If it has a `Test:` reference, find the matching entry in the `tests` array of `tester-results.json`. Verify `status` is `pass`. If `fail`, include the `failure_excerpt` in your finding. If the test is not present in the array, the reference did not resolve to a real test → finding.

3. If it has an `Untestable:` marker, verify the rationale is reasonable.

If the JSON `error` field is non-null:

- Verdict: `fail`.
- Add a single finding naming the error `kind` and `message`.
- Do not flag individual behaviors — the test infrastructure itself failed.

---

## Rules

- **Do not write or run tests.** The Tester Task handles execution. You verify completeness from its output.
- **Do not read source code.** If documentation is insufficient to understand a behavior, report that as a finding.
- **Every new EARS statement needs coverage.** Either a `Test:` reference that passed or an `Untestable:` marker with a reason.
- **A non-null `error` field is a single finding.** When the test infrastructure failed, do not flag individual behaviors.
