2026-06-10 — `FACTORY_MAX_PARALLEL_REVIEWERS` env var is read but
not enforced. The `parallel-attempt-reviewers` Work Item's tests
reviewer flagged this as an advisory finding: the cap value is read
into `_cap` (underscore prefix) and never used, so all planned
review Tasks always spawn unconditionally.
→ Resolved: `915eb3c` (landed directly via fast-forward merge per
the Factory-too-slow override) replaced the `_cap` read with a
`Mutex<usize> + Condvar` semaphore guarded by an RAII `SlotGuard`,
serialized Work Item store access during parallel review with a
`store_lock` mutex, kept review-only Attempts serial, and added a
`cap_enforcement_limits_in_flight_reviewers` test that asserts peak
in-flight count never exceeds the cap.
