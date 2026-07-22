---
name: public-api-surface-test
description: A capability meant for external callers gets a tests/public_api.rs test that compiles only against fluent's public API — constructing a minimal external impl proves the capability is usable without reaching into private internals
metadata:
  type: testing
---

When a change exposes a capability for callers outside the built-in
implementations (an external `Coder`, a public constructor), add or extend
`tests/public_api.rs`. That integration test compiles *only* against `fluent`'s
public API, so it is a compile-time proof that an external caller can use the
capability without naming any private type.

The pattern: implement a minimal stand-in for the external role
(`struct ExternalCoder`), then exercise the public boundary — e.g. construct
`TranscriptCapture::new(transcript_path, project_root)` (which resolves the
project's pump thresholds internally, so the caller never names the crate-private
config type) and thread it through the public `run_captured` boundary. If a
future change makes the capability require a private type, `tests/public_api.rs`
stops compiling.

This complements route/behavior tests: those verify internal wiring;
`public_api.rs` verifies the *surface*. Keep public constructors resolving their
private dependencies internally (`with_config` stays crate-private) so the public
signature does not leak internals. Related:
[[route-tests-drive-real-launch-wiring]], [[backward-compatible-serde-fields]].
