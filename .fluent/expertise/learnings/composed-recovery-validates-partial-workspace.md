---
name: composed-recovery-validates-partial-workspace
description: Compose completion and quota recovery by validating approved partial work separately from the original source candidate
metadata:
  type: architecture
---

A corrective Writer can commit or leave edits before a quota failure. Its
completion-recovery audit still binds the original completed Writer and source
candidate; a later quota approval binds the interrupted Writer's partial HEAD
and workspace contents. Preserve both identities. Requiring the original clean
candidate after quota approval rejects authorized partial work, while merely
detecting a retained audit or instruction marker grants no permission to resume.

At prompt construction, the shared completion contract asks the read-only quota
contract to validate the exact prepared Task, unused launch authority, retained
failure evidence, assignment, and current partial workspace. Accept that partial
workspace only when the approval also names the completion audit's original
source candidate and workspace. Keep the initial correction's clean-candidate
check when no applicable quota approval exists, reject invalid approval bindings,
and leave each recovery's audit intact. Admission and provider spawn retain
their launch-ownership checks.

Composition regressions must reach the matrix-bound committed-source branch.
An older output-free completion repair without a source-matrix digest does not
exercise its candidate check. Start with a committed source, let its corrective
Writer commit and leave edits before quota failure, then approve and resume
through foreground and queued execution. Check that partial work survives and
that changes after approval reject without mutating Work or source evidence.

Related: [[composed-recovery-instructions-bind-each-preparation]],
[[committed-writer-matrix-recovery-authority]],
[[recovery-contracts-form-neutral-boundary]],
[[route-tests-drive-real-launch-wiring]].
