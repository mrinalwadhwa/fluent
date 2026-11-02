---
name: release-host-contract-stays-independent
description: Keep release host obligations outside ordinary Tester and validate their complete candidate-committed contract before publication
metadata:
  type: architecture
---

Ordinary Work and landing run the unattended suites in `.fluent/tester.yaml`.
Release-only host commands live in `.fluent/release-host.yaml`; keeping that
manifest in a checkout does not cause ordinary Tester to execute its commands.
Release Work declares required Plan rows with responsibility `host`, exact
command text, and working directory `.`. Host evidence must pass targeted Tests
Review for the same Work Item, Attempt, and candidate. An authorization
environment variable or a result file alone cannot satisfy those obligations.

Release acceptance reads the manifest from the candidate commit and compares
it with the mandatory commands pinned independently in
`src/release_acceptance.rs`. It checks command completeness, uniqueness,
harnesses, process-settlement requirements, and default applicability before
crediting reviewed evidence. A candidate cannot reduce its release obligations
by deleting or editing its own manifest. Missing manifests remain compatible
only when the candidate's committed legacy Tester contains every mandatory
host command; preserve that historical configuration and its original proof.

`fluent tester verify` inspects retained acceptance without running configured
checks or changing Work state. Release publication must pass this verifier in
addition to ordinary Tester. Test the boundary through the real verifier with
synthetic reviewed host evidence, invalidating each command's bindings and
review independently, and verify that rejection prevents publication.

Related: [[required-checks-outlive-individual-results]],
[[host-evidence-settles-against-bound-writer]],
[[release-verification-uses-exact-clean-checkout]].
