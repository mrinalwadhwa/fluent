---
name: inject-side-effects-for-testability
description: Side-effect functions like notify() must be injected via &dyn Fn parameters so tests can capture and assert on them
metadata:
  type: testing
---

Functions that produce side effects visible only outside the process (OS notifications, stderr writes) must accept the effectful function as a parameter rather than calling it directly. This lets tests inject a capturing closure and assert on the call count and content.

The pattern in this codebase uses `&dyn Fn(&str, &str)` for notification functions. Production callers pass `&crate::notify::notify`; tests pass an `Arc<Mutex<Vec<(String, String)>>>` capturing closure.

A test that calls `notify()` directly without exercising a production code path is a no-op test — it proves the function is callable but not that the production code invokes it. The test reviewer will block on this.

Related: [[backward-compatible-serde-fields]]
