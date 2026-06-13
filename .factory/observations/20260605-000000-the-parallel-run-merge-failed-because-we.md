2026-06-05 — The parallel run merge failed because we committed
to main while child runs were executing. This suggests main should
be protected — no direct commits while runs are active. Consider a
merge queue: an agent that owns merging to main. Child runs and
regular runs produce branches. The merge queue agent rebases,
merges, and optionally spins up new runs to review the merged
result before it lands on main. This is similar to CI merge queues
but the queue agent can be intelligent — resolving simple conflicts,
running targeted reviews on the merged code, and rejecting merges
that break tests. Direct commits to main would be forbidden while
the queue is active.
