---
name: required-checks-outlive-individual-results
description: Retain independent check obligations after proof changes or finding acceptance, regardless of Task ordering
metadata:
  type: architecture
---

Fluent must retain a required check even when its previously accepted proof
becomes invalid. Canonical commands, required Plan rows, and reviewer-requested
commands remain separate obligations. In particular, a reviewer-requested
command absent from the canonical Tester must still block readiness after its
proof expires or changes. Filtering invalid results out of a list of accepted
canonical replacements would silently lose that independent requirement.

Evaluate reuse against the exact candidate, command, working directory, checker
configuration, declared environment and comparison inputs, source artifacts,
and targeted Review. A later no-change Writer or unrelated Task does not itself
invalidate those bindings. Preserve the original producer and Review as
provenance, and reject reuse when a newer conflicting result remains unresolved.
Candidate-code evidence has no age-only expiry; live-state evidence requires an
explicit positive lifetime. Ordinary current-candidate assessment does not
transfer older-candidate proof to a changed candidate, or prevent that
candidate's initial reviews and complete Tester from producing fresh proof.
Landing's narrow expertise-only reuse exception follows the
[reviewed command proof decision](../decisions.md#reuse-reviewed-command-proof-only-for-unchanged-inputs).

Credit a canonical replacement only to its bound command occurrence. Native
test results carry a harness without a command index, so attribute them to an
accepted command only when that harness occurs once in the ordered
configuration. Repeated harnesses require the exact synthetic command-index,
command, and harness identity. Use the same attribution for canonical defects
and named references; apply accepted coverage in a derived result view and
preserve the original failed result bytes. Missing command results, unrelated
structured failures, extractor errors, and unresolved process leaks remain
blockers.

Revalidate accepted carried-finding proof without returning overall recovery
success. Its acceptance satisfies only that finding; the shared recovery
assessment must continue to later active recoveries, expired proof, canonical
integrity blockers, and outstanding Tester checks. Invalid accepted proof still
returns an error. Otherwise an early success can hide the next attachment command
and give status and queue admission an incorrect view of the remaining work.
Exercise acceptance followed by an unrelated canonical host requirement through
status, foreground resume, queue admission, fresh attachment, and final readiness.

Regression tests must invalidate proof after targeted acceptance while another
requirement remains outstanding, then try to finish that other requirement.
Include an independent reviewer command absent from canonical configuration;
using a canonical command for every fixture can hide a lost obligation.

Related: [[expired-evidence-renews-through-explicit-attachment]],
[[host-evidence-settles-against-bound-writer]],
[[no-change-output-binds-preserved-commit]],
[[negative-test-cross-product-coverage]].
