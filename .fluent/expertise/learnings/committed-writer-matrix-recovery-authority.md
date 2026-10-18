---
name: committed-writer-matrix-recovery-authority
description: Distinguish matrix-bound Writer-completion recovery from older audits by its source digest and operation Task binding
metadata:
  type: architecture
---

Writer-completion recovery keeps two compatible authority shapes under the
persisted schema. Older audits omit `source_matrix_sha256` and bind the recovery
operation to the Task named by `task_id`. An audit for a completed Writer's
malformed matrix, unfinished no-change work, or proven stagnation after committed
continuations includes `source_matrix_sha256`: `task_id` names the new corrective
Writer, while the operation binds `source_writer_task_id`, the completed source Writer.
The optional digest selects this interpretation; do not infer it from Task
order or change the binding for older audits.

The committed-matrix authority also binds the immutable requirement contract,
source assignment, transcript, output, candidate, and original matrix bytes.
The source matrix remains in the completed Task's artifact area; the audit
stores its digest rather than a copy. Revalidate the bound evidence when
authorizing or replaying the recovery and before the corrective Writer starts.
Keep the completed source Task and its artifacts intact, and give the corrective
Writer a fresh matrix from the contract. Approval schedules a Writer; a
separate resume executes it.

For a parseable incomplete committed matrix, preserve the saved-history admission
contract recorded in [decisions.md](../decisions.md#keep-saved-writer-history-aligned-with-stagnation-recovery).
The pause label and pending rows alone do not prove stagnation. An earlier
continuation may be a verified no-change run, but its saved declaration must
match its output and unchanged commit. Every later Writer's base must match
the preceding Writer's commit; individually valid outputs do not establish a
connected history.

For stalled or unfinished no-change recovery, partial claims on pending
Writer-owned rows do not grant completion credit or prevent eligibility by
themselves. Validate a copy with those rows' claims cleared, while still
checking immutable rows and completed claims against the contract and findings.
Retain the original source bytes and their audit binding. This eligibility
view neither repairs the published matrix nor relaxes the evidence required
when the corrective Writer marks a row complete.

A valid no-change output does not itself finish every completion requirement.
Validate the matrix against the immutable contract and review findings, then
schedule a separate Writer only when a row assigned to the Writer remains
pending. Pending host-owned, review, Tester, or Learner work follows its own
route. A historical no-change approval that reserved no successor remains
valid history; a distinct current decision must bind the source before
scheduling a new Task. Foreground resume and queued admission and claim must
revalidate that authority before launch.

Related: [[recovery-contracts-form-neutral-boundary]],
[[reused-recovery-authority-revalidates-publication]],
[[composed-recovery-validates-partial-workspace]],
[[corrective-writers-preserve-candidate-lineage]].
