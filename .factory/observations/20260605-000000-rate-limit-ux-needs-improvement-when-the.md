2026-06-05 — Rate limit UX needs improvement. When the user hits
Anthropic's usage limit: (1) the dashboard should show a countdown
to next retry, not just a static "Rate limited" label, (2) a
notification should tell the user things paused but aren't broken,
(3) the session loop should respect Retry-After headers rather
than using a fixed 5-minute wait, (4) multiple concurrent runs
should stagger retries to avoid thundering herd on the rate limit.
