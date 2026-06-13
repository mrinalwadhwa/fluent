2026-05-13 — On the Fargate test, round 2 reviewers all crashed
(exit 1) after round 1 had 5 reviewers + author session 2. Cause
unknown — could be rate limits, container resource exhaustion, or
something else. Needs investigation with reviewer transcripts next
time it happens.

→ Resolved: Obsolete. Slice 3 removed merge-time reviewers entirely; the Work-model Fargate path runs post-merge reviews via the debounced queue, not synchronous merge-time reviewers. The 'round 2 reviewers all crashed' scenario is no longer reachable in the current architecture.
