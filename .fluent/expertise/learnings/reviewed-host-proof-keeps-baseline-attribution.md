---
name: reviewed-host-proof-keeps-baseline-attribution
description: Compose reviewed host proof with target-baseline failures while checking original and landing results independently
metadata:
  type: architecture
---

Capture landing may combine accepted host proof for one canonical Tester
command with unrelated failures explained by the retained target baseline.
Both the live landing comparison and retained post-land assessment pass that
baseline to the shared proof check in `tester_evidence`. Preserve the
expertise-only ancestry and unchanged-input restrictions described in
[[required-checks-outlive-individual-results]].

Check the original proof-bound Tester result and the current landing result
independently. Every other failed command must satisfy the shared baseline
classifier, including its command occurrence, text, harness, exit code, native
failures, and process settlement. Host proof for one command cannot explain a
second command's new failure.

Check native failures independently of command exits, using both test ID and
harness. Every original and current native failure must belong to the baseline
or be attributable to the reviewed command. Outside the baseline, a current
failure attributed to that command must also appear in its original result.
An unrelated original failure still blocks reuse when the landing result no
longer contains it, including when its command reports exit zero.

Exercise this composition with a baseline present and vary original and current
native failures separately while keeping command identities and proof bindings
valid. Retained-history tests must reach the production history assessment with
its managed artifacts and landing intent. When changing a test result to check
failure classification, recompute the fixture's landing evidence digest and
assert the classification diagnostic so an integrity rejection cannot mask the
rule under test.

Related: [[baseline-attribution-does-not-satisfy-references]],
[[capture-landing-owns-exact-tester-evidence]],
[[semantic-validation-tests-reach-target-branch]].
