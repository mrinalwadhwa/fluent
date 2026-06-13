2026-06-08 — A later Work Attempt follow-up review policy pass should
decide whether passed reviewers receive scoped stale-review context or
whether touched domains, broad shared changes, and explicit review policy
should add reviewers back into intermediate Attempt rounds. Factory now
passes role-matched failed review artifacts into targeted follow-up
review Tasks; the remaining question is when selective review is too
narrow before the full merge-time reviewer set runs.

→ Resolved: Covered by the post-merge review queue. The narrowed re-review tradeoff (selective review may miss something a passed reviewer would catch) is mitigated by the post-merge review queue, which runs the full reviewer set on every merged commit and auto-creates post-merge-review-fix Work Items for any findings. The observation referenced the legacy 'merge-time reviewer set' (synchronous, removed in slice 3); post-merge review is its async replacement.
