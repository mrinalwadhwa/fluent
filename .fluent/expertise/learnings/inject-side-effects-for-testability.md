---
name: inject-side-effects-for-testability
description: Side-effect functions like notify() must be injected via &dyn Fn parameters so tests can capture and assert on them
metadata:
  type: testing
---

Functions that produce side effects visible only outside the process (OS notifications, credential refresh, stderr writes) must accept the effectful function as a parameter rather than calling it directly. This lets tests inject a capturing closure and assert on the call count and content.

The pattern in this codebase uses `&dyn Fn` parameters with signatures matching the side effect: `&dyn Fn(&str, &str)` for notifications, `&dyn Fn()` for fire-and-forget operations like credential refresh. Production callers pass the real function (e.g., `&crate::notify::notify`, `&real_credential_refresh`); tests pass no-op closures (`&|_, _| {}`, `&|| {}`) or `Arc<Mutex<_>>` counting/capturing closures.

Tests that don't assert on a particular side effect should still inject a no-op closure rather than the real function, to keep the test hermetic. The three non-notification auth tests, for example, pass `&|_, _| {}` for `notify_fn` in addition to a fake `refresh_fn`.

A test that calls a side-effect function directly without exercising a production code path is a no-op test — it proves the function is callable but not that the production code invokes it. The test reviewer will block on this.

Related: [[backward-compatible-serde-fields]]
