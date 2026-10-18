---
name: approval-records-bind-current-authority
description: Bind approvals to current project authority and validate their exact Work revision at the host mutation boundary
metadata:
  type: architecture
---

Keep lifecycle approval policy pure. The coordinator loads and validates the
current project authority, passes that snapshot and the pending action into the
domain policy, persists the returned aggregate, and emits any Slack intent. The
policy must not load configuration, access the filesystem, or perform the
authorized planning, execution, resume, or landing effect.

An accepted approval becomes a self-contained durable authorization record. It
binds the project and thread, pending action id and kind, artifact revision and
presentation time, approving actor, acceptance time, and inbound event identity.
Clear the pending action only after creating this record. Effect consumers use
the immutable authorization record rather than reconstructing authority from
mutable thread state or current configuration.

Acceptance does not prove that the effect completed. Apply approved planning
revisions and execution authorization with an operation identity and the observed
Work fingerprint at the host mutation boundary. Check that fingerprint under
the Work model lock; a runner's conversation lock does not serialize local CLI
or scheduler changes. Publish a
model effect and its immutable operation receipt in the same recoverable Work
transaction. Reconcile a matching effect receipt without reapplying the effect
or overwriting later revisions. An admission receipt alone cannot prove an
external effect completed or waive the revision check on retry.

For queue resume, also bind the observed dispatch state and retain the operation
identity on the admitted dispatch, so replay cannot authorize a later stop.
Landing retains its host revision checks and existing candidate, evidence,
cancellation, and landing-intent boundaries. Reject stale unapplied approvals
without clearing a newer pending control.

Test the complete record, not only that an approval count increased or the
pending action disappeared. Assert every binding field and replay the same event
identity to prove the decision is idempotent. Interrupt both before and after
the effect, introduce a newer Work revision or cancellation, and verify that
recovery preserves it. Related:
[[pure-domain-keeps-filesystem-authority-at-edges]],
[[token-bound-to-persisted-record]],
[[publish-managed-inputs-with-model-record]],
[[scheduler-resume-requires-explicit-dispatch-intent]].
