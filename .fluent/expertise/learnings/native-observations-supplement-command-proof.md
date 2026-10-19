---
name: native-observations-supplement-command-proof
description: Preserve command acceptance when separately reviewed native observations supply named-test evidence
metadata:
  type: architecture
---

Native observations supplement an already accepted canonical command through a
separate targeted Tests Review. Persist this supplementary `EvidenceRecovery`
with `required_check: None` and place its
`NativeObservationRecoveryBinding` in the target's `prior_review_artifact`.
Setting `required_check` on this recovery would make latest-required-check
selection treat it as replacement command proof.

Bind the supplement to the original command recovery, candidate, canonical
command occurrence, working directory, harness, and source Tester artifact.
Revalidate the original command acceptance as well as the supplementary
attachment and its completed passing Tests Review before crediting observations.
A command pass alone supplies no named-test identities.

Keep the canonical Tester bytes and original command attachment unchanged.
`verification_assessment::accepted_native_observation_tests` supplies a derived
test-results view to both current assessment and candidate gating; the existing
native identity matcher decides which named references that view satisfies.
Changes to generic recovery handling must preserve this separation between
command acceptance and supplementary named-test evidence.

Related: [[required-checks-outlive-individual-results]],
[[recovery-contracts-form-neutral-boundary]],
[[rust-test-evidence-preserves-native-qualifiers]].
