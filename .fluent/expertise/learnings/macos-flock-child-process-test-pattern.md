---
name: macos-flock-child-process-test-pattern
description: macOS flock tests must spawn child processes via current_exe, not threads; a sentinel test function guards the child entry point
metadata:
  type: testing
---

On macOS, `flock` locks are process-associated: a thread that acquires a lock
and another thread in the same process share it. Thread-only tests therefore
cannot prove cross-process serialization and will pass even when the lock is
absent.

The accepted pattern in this codebase is:

1. In the integration test file, add a sentinel test function (e.g.,
   `__registry_child_worker`) that returns immediately when a designated env
   var (e.g., `FLUENT_REGISTRY_CHILD`) is absent and performs the real child
   work when the var is set.
2. The concurrent test spawns N child processes via `std::process::Command::new(
   std::env::current_exe()?)`, passing `--test <sentinel_name>` and the env
   var, so each child runs the sentinel under a fresh process boundary.
3. The parent waits for all children, then verifies that every mutation
   survived — proving the lock serialized all concurrent writers.

The sentinel test is a no-op in CI's normal nextest run (env var absent), so it
adds no observable side effect when not invoked by the concurrent test.

Related: [[registry-read-modify-write-needs-flock]], [[production-lock-test-hooks]]
