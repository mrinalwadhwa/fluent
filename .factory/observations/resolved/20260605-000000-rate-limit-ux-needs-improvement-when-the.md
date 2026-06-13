2026-06-05 — Rate limit UX needs improvement. When the user hits
Anthropic's usage limit: (1) the dashboard should show a countdown
to next retry, not just a static "Rate limited" label, (2) a
notification should tell the user things paused but aren't broken,
(3) the session loop should respect Retry-After headers rather
than using a fixed 5-minute wait, (4) multiple concurrent runs
should stagger retries to avoid thundering herd on the rate limit.

Resolved 2026-06-12 — Work Item rate-limit-ux. Items (2), (3), and
(4) addressed: the Coder wrapper now parses structured rate-limit
events from transcripts (Claude Code and Codex), applies per-run
jitter to stagger concurrent retries, and fires macOS notifications
on rate-limit state transitions. Item (1) — dashboard countdown —
deferred to the dashboard overhaul Work Item.
