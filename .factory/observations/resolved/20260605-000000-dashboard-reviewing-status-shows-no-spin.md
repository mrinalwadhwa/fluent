2026-06-05 — Dashboard "reviewing" status shows no spinner in the
header. compute_phase needs to map "reviewing" to animated=true.
Also, reviewer tabs show stale verdicts from the previous round
instead of resetting to "running" when a new review round starts.
The dashboard needs to detect that review artifacts have been
archived (moved to round-N/) and reset reviewer status accordingly.
→ Resolved: 04b083a, 307c112, a6b8f8a, bae62ca, 5a46c92 (dashboard
tracks the current review round, refreshes reviewer transcript state
for the active round, and has deterministic behavior coverage)
