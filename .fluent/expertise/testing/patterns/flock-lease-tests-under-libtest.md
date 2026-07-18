# Test flock-based leases with a bounded re-acquire retry

## Title

Tolerate macOS flock release-visibility latency when testing
`lease`-based singletons in-process.

## Context

`src/lease.rs` provides advisory locks via `flock(2)`, released when the
holding descriptor closes. These model **process-level** ownership: a
land process and a separate daemon process contend for the same lock.
The project's declared harness is `cargo nextest` (process-per-test),
which mirrors production faithfully.

When you also want the tests green under `cargo test` (libtest's
thread-per-test harness), you hit a macOS quirk: after a holder's
descriptor closes (drop the `TaskLease`, or `is_leased`'s
acquire-probe-then-unlock), a fresh `flock(LOCK_EX|LOCK_NB)` on a new
descriptor in the same thread can **briefly still see the lock held**
under concurrent thread load. The "is it held" direction is reliable;
only the "it became free after release" direction races. This surfaces
as flaky failures on assertions like "a later daemon can acquire the
released lease".

## Mechanism

- Assert the **held** direction directly and once — while a live
  `TaskLease` is alive, `acquire_*` returns `None` deterministically.
- Assert the **freed-after-release** direction through a bounded retry:
  loop `acquire`, sleeping a few ms between tries, up to a small cap.
  Under nextest and in production the first try succeeds; under libtest
  the retry absorbs the release-visibility window.
- Avoid chaining `is_leased` immediately before `acquire` in a test —
  `is_leased`'s acquire-then-unlock probe widens the race window.

## Example

```rust
// Retry to tolerate macOS flock release-visibility latency under the
// thread-per-test harness. Separate processes release synchronously.
fn acquire_lease_eventually(root: &Path, branch: &str) -> Option<TaskLease> {
    for _ in 0..50 {
        if let Some(lease) = acquire_daemon_lease(root, branch) {
            return Some(lease);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    None
}

#[test]
fn daemon_releases_lease_on_exit() {
    let tmp = TempDir::new().unwrap();
    let lease = acquire_daemon_lease(tmp.path(), "main");
    assert!(lease.is_some());
    // Held direction: deterministic, single check.
    assert!(acquire_daemon_lease(tmp.path(), "main").is_none());
    drop(lease); // models the daemon exiting
    // Freed direction: bounded retry.
    assert!(acquire_lease_eventually(tmp.path(), "main").is_some());
}
```
