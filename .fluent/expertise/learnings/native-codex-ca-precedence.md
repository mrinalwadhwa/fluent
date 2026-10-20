---
name: native-codex-ca-precedence
description: Native Codex preserves explicit SSL_CERT_FILE settings before selecting a shared file-based CA bundle
metadata:
  type: architecture
---

Native Codex and sandboxed Codex exec share the CA bundle selector in
`src/coder.rs`, but apply different environment precedence. Native Tasks supply
a fallback `SSL_CERT_FILE` only when neither the process environment nor
`launch_env` contains that key. Presence suppresses the fallback even when the
explicit value is empty. The shared selector prefers an existing file named by
`FLUENT_CODEX_CA_BUNDLE`, then checks the supported system bundle locations.
The sandboxed exec path can replace a caller's `SSL_CERT_FILE` with the selected
bundle; do not infer native precedence from that path.

File-based certificate trust lets sandboxed Codex connect without Keychain IPC.
Preserve the production worker sandbox's interactive-home and Keychain denials
when changing certificate setup. An authenticated native host check with the
caller's `SSL_CERT_FILE` unset exercises fallback startup; helper tests alone
cannot establish that the real provider completes its request.

Related: [[native-input-settlement-binds-identity]],
[[terminal-host-tests-exercise-live-boundaries]].
