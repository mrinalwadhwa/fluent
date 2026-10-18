---
name: host-evidence-settles-against-bound-writer
description: Settle host-evidence reviews against their recorded Writer candidate while requiring fresh evidence for a later candidate
metadata:
  type: architecture
---

A Plan host-evidence receipt can remain `Reviewing` after its targeted Tests
Review passes, including in saved history that already contains a later
corrective Writer. Validate that receipt against its recorded completed Writer,
candidate, source matrix, Plan row, command, evidence digest, and targeted Review.
Resolving its source through the latest Writer instead can reject valid history
and prevent fresh evidence from replacing the still-active receipt.

Keep historical settlement separate from current-candidate admission. Ordinary
Attempt resume and queue admission require the receipt's candidate and Plan
requirement to match the current Writer and completion matrix. Only fresh
explicit Plan attachment may settle a completed older-candidate Review as
history, after validating its original Writer, source, evidence, and Review.
That settlement cannot credit the current candidate, which still needs fresh
host proof. Historical settlement validates the source Writer's matrix and
does not depend on the later Writer's matrix being ready; current admission
still requires that later matrix. Reject changed evidence or previously
recorded Review bytes even when the saved verdict still says pass.

For a matching current candidate, the Attempt loop settles passing Plan Reviews
inside a fresh Work-model mutation and records their digests. Queue admission
checks the same facts without rewriting history; attachment revalidates within
its transaction. Preserve original evidence and Tasks throughout both routes.

Recovery tests must complete the corrective Writer, reach the fresh host handoff,
attach evidence, and resume. Stopping at Writer materialization misses the active
receipt's interaction with later admission.

Related: [[audited-planning-recovery-binds-exact-frontier]],
[[recovery-contracts-form-neutral-boundary]],
[[fresh-field-level-finalizer-preserves-concurrent-state]].
