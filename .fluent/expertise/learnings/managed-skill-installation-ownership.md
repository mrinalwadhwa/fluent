---
name: managed-skill-installation-ownership
description: Fluent replaces a materialized skill only when complete provenance or an exact legacy inventory proves ownership; every other installation remains user-owned
metadata:
  type: architecture
---

`fluent skills add` has an ownership boundary, not a best-effort overwrite
policy. Fluent may replace a skill directory only when either its
`.fluent-managed.json` sidecar is self-consistent (schema and identity match,
its complete file inventory matches the tree, and the digest matches the
recorded bytes) or the sidecar-free directory exactly matches a checked-in,
provenanced historical migration fixture.

Treat the complete tree as identity. For legacy migrations, the deterministic
digest must distinguish regular files from directories and include their paths
and file bytes; root or nested symlinks and other special entries reject the
migration. For managed installations, compare actual files and directories
against the sidecar-derived inventory before accepting its digest. An edited,
missing, added, malformed, or identity-mismatched entry therefore withdraws
Fluent's authority to replace the directory. Preserve it unchanged and report
the path with manual-cleanup guidance.

The migration fixture is itself release authority: retain immutable source
provenance and keep the note outside directories whose bytes contribute to the
allowlist digest. Exercise this boundary through the CLI with an exact legacy
bundle, an added empty directory, and a symlink, so tests prove both safe
adoption and non-destructive conflict handling.
