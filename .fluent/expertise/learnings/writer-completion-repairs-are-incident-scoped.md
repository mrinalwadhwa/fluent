---
name: writer-completion-repairs-are-incident-scoped
description: Bind each Writer-completion repair to its current source and treat settled earlier repairs as validated history
metadata:
  type: architecture
---

A Writer-completion approval authorizes one source Writer and candidate, not
every later failure in the same Attempt. When a completed Writer has another
eligible completion pause, select that current source before deciding whether
an audit is a replay. The completed replacement from an earlier repair can itself
be the new stalled source. Validate earlier attached audits and their completed
replacement Tasks as history, then require a distinct decision and new audit, operation,
and corrective Task for the later incident. Preserve both historical authority
shapes: older output-free audits and committed-matrix audits have different
source and operation bindings.

Status must use the current incident's eligibility and validated history before
offering a repair command. A valid no-change completion with no pending
Writer-owned work continues on its host-evidence route. Public-route tests
should cover a later repair after both authority shapes, including intervening
targeted Review history, exact replay, stale evidence, and the action shown by
status.

Related: [[committed-writer-matrix-recovery-authority]],
[[next-action-guidance-is-an-executable-interface]],
[[recovery-contracts-form-neutral-boundary]].
