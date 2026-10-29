---
name: lease-observers-serialize-probes
description: Serialize lease observers on the existing parent directory before probing ownership, and retain uncertainty on infrastructure errors
metadata:
  type: architecture
---

`src/lease.rs` checks ownership with a nonblocking exclusive flock so it can
detect both shared and exclusive owners. Concurrent observers must not mistake
another observer's temporary probe for a live owner. Each observer first takes
a blocking exclusive flock on the lease file's existing parent directory, then
probes the lease file while holding that directory guard. Owners acquire the
lease file directly; the directory lock coordinates observers only.

Observation opens existing paths without creating lock files or changing
persisted Work state. A missing lease file returns false. Other open or flock
failures propagate through `is_leased_checked`; the boolean `is_leased` returns
true on those failures because uncertainty cannot establish settled ownership.
Keep this conservative observation policy distinct from typed acquisition,
which reports infrastructure failure separately from actual contention.

Related: [[lease-acquire-types-contention-vs-infrastructure]],
[[lock-ordering-across-subsystems]].
