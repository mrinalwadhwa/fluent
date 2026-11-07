# Build persisted merge-ready fixtures

## Context

Use this when a dashboard action test must offer or dispatch Candidate show or
Candidate land through fresh source reads. Setting an Attempt's status and
creating a Merge Candidate alone does not establish local `merge-ready` status.

## Mechanism

Create a real Git commit in the temporary checkout. Complete the Writer using
its default managed workspace binding and the full commit SHA. Add ordinary
review Tasks through the Work model, complete them, and write their passing
review artifacts. Record successful Learning, create the Merge Candidate, and
persist the aggregate through `WorkModelStore`.

Reload `SourceSnapshot` and require `merge-ready` before driving action keys.
Local status authenticates the review bindings against retained Git objects;
summary flags or abbreviated placeholder commits instead yield
`landing-blocked`. Use the fake dashboard runner to inspect exact arguments and
the canonical checkout without landing Work.

When testing a changed candidate commit, create another real commit and update
the persisted Writer output and review contexts to match. Require the fresh
record to remain `merge-ready` before checking that the dashboard rejects the
old confirmation. This isolates stale identity from loss of action eligibility.

## Example

`src/dashboard/tests.rs::merge_ready_action_fixture` supplies the persisted
state for the `project_action_candidate_*` tests. They drive selection,
preparation and dispatch, including immediate read-only show, explicit land
confirmation, decline, one launch per confirmation, and changed candidate
identity or commit before and after confirmation.
