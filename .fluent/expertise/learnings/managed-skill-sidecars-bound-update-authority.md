---
name: managed-skill-sidecars-bound-update-authority
description: A managed skill can be replaced only when a valid identity-scoped sidecar accounts for its complete on-disk inventory; every other installation remains user-owned
metadata:
  type: architecture
---

`fluent skills add` treats a skill directory as Fluent-managed only when its
`.fluent-managed.json` sidecar validates the schema, selected agent, scope,
skill identity, digest, and complete file inventory. A valid sidecar lets a
newer Fluent binary update an earlier managed bundle without retaining an
allowlist of every prior release digest.

Any malformed, mismatched, changed, missing, or unlisted content withdraws
that authority. Preserve the directory byte-for-byte and report cleanup
guidance rather than replacing it. Apply this precedence before every legacy
adoption route, including secondary scans for shims: a sidecar-bearing
directory never becomes adoptable merely because it resembles a known prior
bundle or Fluent-marked shim. Only sidecar-free, exact allowlisted legacy
bundles and sidecar-free marked shims are bounded migration targets.

Related: [[atomic-write-replace-through-utility]],
[[route-tests-drive-real-launch-wiring]].
