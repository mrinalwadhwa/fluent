---
name: no-change-output-binds-preserved-commit
description: A no-change Writer preserves its verified base commit; capture Learning may advance the current commit only with typed, post-acceptance canonicalization provenance
metadata:
  type: architecture
---

A follow-up Writer may complete without a new commit only by proving that it
preserved the previous completed Writer commit. In the durable `TaskOutput`,
`base_commit` permanently names the commit that Writer verified, `commit` names
the current candidate, and `no_change` records the typed verification evidence.
Those two commit identities initially match. Capture Learning may later advance
`commit` to a canonical expertise result, but it must preserve `base_commit` and
attach typed host-owned provenance that names the exact transition from the
verified commit to the canonical commit.

Treat that divergence as a cross-object lifecycle invariant, not as a locally
self-consistent exception. Aggregate Work-model validation accepts it only when
all related state proves Fluent accepted the canonical result: the Attempt uses
capture mode, the output belongs to the latest completed Writer, the Merge
Candidate names the transition destination, and Learning is post-acceptance
(`HandoffPending`, `Succeeded`, or a typed handoff-publication failure after
canonicalization). An `InProgress` reservation or an ordinary coder,
transcript-pump, or evidence failure cannot authorize divergence. A generic
`Failed` status is not enough because it conflates failures from before and after
canonicalization; persist the host-owned failure stage that distinguishes the
accounted recovery state.

The evidence belongs to the Write Task that produced it. Its declaration path
must resolve to that Task's artifact area, its schema version must be supported,
and its reason and passing verification commands must be non-empty. The executor
accepts it only for a follow-up Writer whose workspace is clean and whose HEAD
still equals the preceding completed Writer commit. A Writer that advances HEAD
uses ordinary committed output and must not retain the no-change marker.

Treat the declaration as launch-scoped authority, not reusable state. Remove
only that declaration before every coder launch, including retries, and propagate
cleanup errors. Otherwise a failed launch can leave evidence that incorrectly
authorizes a later launch which performed no verification.

Cover the contract at both persistence and execution boundaries. Model/storage
tests reject missing, reversed, partial, or mismatched transitions and vary each
lifecycle precondition independently. A real Attempt-route test must run the
actual no-change Writer before capture Learning, create a canonical expertise
commit, and prove that the verified commit, current commit, transition, Merge
Candidate, model validity, and landing readiness remain aligned. A fixture that
mutates a completed output into a no-change shape does not test Writer
finalization or the Writer-to-Learner handoff. Keep the rendered follow-up Writer
prompt free of unconditional commit requirements so its instructions agree with
the host-enforced path.

Related: [[backward-compatible-serde-fields]],
[[mode-specific-prompts-replace-conflicting-base-instructions]],
[[route-tests-drive-real-launch-wiring]].
