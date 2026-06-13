2026-06-08 — Work Attempt follow-up review Tasks should receive the
prior failed review artifacts that led to the follow-up write. Factory
already reran only the failed reviewer roles that fed a follow-up write
Task, while keeping the full reviewer set as the merge-queue safety gate,
but reviewers still had to rediscover the concrete prior findings.
→ Resolved: 2156c34 and 885c9de. Factory now maps a completed
follow-up write Task's failed review input artifacts back to reviewer
roles, attaches the role-matched artifact to each targeted follow-up
review Task, includes those artifact paths and read-first guidance in
review prompts, and grants sandboxed read access to the prior review
artifact directories. Behavior, architecture, and binary tests cover the
new review input flow while merge-time reviews still run the full
reviewer set.
