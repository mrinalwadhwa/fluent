---
name: hermetic-release-provider-boundary
description: Declared release suites isolate nested provider routes with fail-closed doubles and an explicit fixture credential allowlist
metadata:
  type: testing
---

The declared release suites must not allow a nested Fluent launch to reach an
operator-installed Claude, Codex, or Pi executable, or inherit operator provider
credentials. Enable the test-support hermetic-provider marker for the suite, put
`tests/fixtures/provider-doubles` ahead of the inherited `PATH`, and remove the
known provider credential variables from every test-owned coder child.

A fixture that intentionally exercises one authentication route must name that
single credential through the fixture-only allowlist; it must not disable the
boundary or inherit the rest of the operator environment. Provider doubles may
answer only their supported readiness probe and must reject every other
invocation. This makes an incomplete fixture fail closed instead of silently
using a live provider.

Route coverage must drive each supported provider through a real nested launch
and assert the corresponding double's rejection diagnostic. A readiness-only or
configuration-only test cannot prove the executable and credential boundary
reaches the child process. Related: [[route-tests-drive-real-launch-wiring]] and
[[provider-readiness-crosses-reservation]].
