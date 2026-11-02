---
name: snapshot-git-reads-stay-in-load-scope
description: Reuse only successful fully addressed Git object reads within one synchronous local snapshot load
metadata:
  type: architecture
---

The Git module owns the local dashboard snapshot's temporary object-read cache.
Each `SourceSnapshot::load` activates an empty thread-local scope; its non-Send
guard restores the previous scope on return or unwind. The scope does not
propagate to spawned threads. Revisit this ownership contract before moving
projection work onto other threads.

Only successful `git show FULL_OBJECT_ID:path` queries qualify, with a full
40- or 64-character hexadecimal object ID and a nonempty path. Key retained raw
outputs by the canonical working directory and complete argument list. Retain
at most 64 MiB of stdout and stderr bytes per scope. Failed queries, mutable
references, abbreviated IDs, options, and explicit environment-override routes
remain uncached; ordinary callers outside the scope execute Git normally.

Reuse source bytes only. Recompute Work status, verification, recovery, queue,
and terminal projections on each load rather than retaining lifecycle decisions
or persisting cached results. Profiling uses separate, feature-gated observation
state and does not supply projection authority.

The real-repository cache regression checks subprocess counts, repository
separation, ineligible queries, fresh scopes, and unwind cleanup. Snapshot
regressions compare full local projections, canonical status, refreshed inputs,
and unchanged stored bytes. Preserve these boundaries when extending reuse.

Related: [[git-subprocesses-use-central-wrapper]],
[[dashboard-local-refresh-owns-no-ui-state]],
[[dashboard-source-projection-keeps-typed-state]].
