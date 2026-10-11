---
name: behavior-script-guards-scan-fixture-text
description: Operation-script binary-override guards scan embedded fixture text as well as real CLI invocations
metadata:
  type: gotcha
---

`tests/behaviors/operations/test-behavior-bin-override.sh` scans the operations
directory as text. It rejects lines containing `target/debug/fluent` without
`FLUENT_BIN_OVERRIDE` on the same line, and rejects `cargo run` references.
Embedded Python, heredocs, and comments participate in this check even when
they only describe or create a fake build output and never invoke Fluent.

Operation scripts must honor `FLUENT_BIN_OVERRIDE` when selecting the real CLI.
When a Cargo double must fabricate the expected build output, keep that double
under `tests/fixtures/` and copy it into the scenario's private directory.
Preserve the guard and keep real Fluent launches on the override path; moving
an actual CLI invocation into a fixture does not satisfy that convention.

Run the binary-override guard alongside a changed operation script when adding
build or executable doubles. A passing isolated fixture test does not establish
that its source satisfies the directory-wide check.

Related: [[shell-tests-invisible-to-compiler]].
