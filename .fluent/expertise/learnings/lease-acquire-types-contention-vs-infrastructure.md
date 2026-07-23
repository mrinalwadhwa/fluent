---
name: lease-acquire-types-contention-vs-infrastructure
description: Lock/lease acquisition must return a typed result — only a non-blocking flock WouldBlock is a live peer (fail open); every other lock-path IO error propagates as a real failure
metadata:
  type: gotcha
---

Lock/lease acquisition (`crate::lease`) must return a **typed** result that
distinguishes contention from infrastructure failure — e.g.
`LeaseAttempt::{Acquired, Contended}`, not a bare `io::Result`. Only a
non-blocking `flock` that returns `WouldBlock` (`EWOULDBLOCK`) means a live peer
holds the lock; that is the *sole* case a caller may read as "someone else is
running" and fail open (skip this pass, refresh, retry later). Every other error
on the lock path — `create_dir_all` on the parent (permission / not-a-dir /
disk), `OpenOptions::open` (permission / disk), a read-only filesystem, disk
full, or any non-`WouldBlock` `flock` error — is a genuine infrastructure fault
and must propagate to the caller.

A bare `Err(_) => { refresh; return Ok(()) }` at the call site collapses all
three failure classes into "busy peer." An obstructed lock parent then
masquerades as contention and the guarded work is silently skipped — for the
Learner lease, that silently stalls all advancement (which now *requires* a
succeeded Learner) with no surfaced error. That hidden-failure class is exactly
what lock-gated hardening exists to remove.

This is the fail-closed principle ([[config-fails-open-only-for-diagnostics]])
applied to locks: only true contention fails open; infrastructure faults fail
closed and propagate. Cover it with three unit tests (acquire / contention /
infra-failure) plus a route-level test proving an infrastructure failure launches
no child and leaves durable state unchanged. Related:
[[production-lock-test-hooks]], [[lock-ordering-across-subsystems]].
