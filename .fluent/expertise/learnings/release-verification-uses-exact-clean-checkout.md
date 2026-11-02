---
name: release-verification-uses-exact-clean-checkout
description: Verify retained release host proof from the exact clean release checkout after landing retires the Writer workspace
metadata:
  type: architecture
---

Release acceptance may run after landing has retired the managed Writer
workspace. Its read-only host-completion assessment therefore passes the
release checkout explicitly to
`recovery_contracts::owned_host_completion_matrix_at`. That checkout must be
clean and at the exact recorded Writer candidate commit. Retained source
artifacts, evidence bindings, and targeted review still require validation;
the alternate checkout supplies no new verification credit.

Ordinary recovery calls `owned_host_completion_matrix`, which supplies no
alternate checkout and continues to require the original managed candidate
workspace. Keep the release-only caller explicit rather than making an
arbitrary available checkout a general fallback. Release inspection verifies
retained proof; recovery authorizes further execution against managed source.

Exercise retained release verification both before and after landing through
the public CLI, asserting that it neither executes checks nor changes retained
Work artifacts.

Related: [[release-host-contract-stays-independent]],
[[recovery-contracts-form-neutral-boundary]],
[[host-evidence-settles-against-bound-writer]].
