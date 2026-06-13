2026-06-10 — A coder rate-limit response should not become a hard
Task failure. When Claude returned `You've hit your session limit · resets 7:10pm`,
Factory recorded the exit code as a Task failure and marked the
Attempt `failed`. The conversation agent then had to cleanup,
re-create the Work Item, and re-run.
→ Resolved: `27c8fbd` added `transcript_indicates_rate_limit` and
`run_with_transcript_retrying`. Coder runs whose transcripts contain
session-limit or rate-limit markers now sleep
`FACTORY_RATE_LIMIT_RETRY_AFTER_SECS` (default 1800) and retry up to
2 times before propagating the exit code. Author and reviewer Tasks
inherit the retry without a Coder trait surface change.
