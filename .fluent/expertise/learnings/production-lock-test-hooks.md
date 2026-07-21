---
name: production-lock-test-hooks
description: Keep lock-contention pause hooks test-only, scoped, bounded, and panic-safe
metadata:
  type: gotcha
---

Lock-contention tests need deterministic phase handshakes, but a production lock path must not consult ambient variables that can pause it. Release-reachable `FLUENT_TEST_*` waits let any caller that controls the environment hold a lineage or operation lock forever.

Compile in-process probes only under `#[cfg(test)]`. Key each probe to the exact lock channel and identity, give every wait a timeout, and let the scoped probe release blocked workers in `Drop`. Install the probe inside the `std::thread::scope` closure so panic unwinding releases it before the scope joins worker threads. Tests still call the real lock acquisition path; only the phase notification disappears from release builds.

Cross-process tests that cannot use a unit-only probe should observe real public effects or nonblocking contention. A write-only marker may report a phase, but production code must never wait for a test-created release file.

Related: [[lock-ordering-across-subsystems]]
