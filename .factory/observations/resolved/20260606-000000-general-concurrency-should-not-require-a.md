2026-06-06 — General concurrency should not require a parent run.
Factory currently models most parallel work as one parent plan that
spawns child runs and owns the group merge. That is useful for
decomposing a single large brief into dependent or synthesized pieces,
but it is the wrong default for five unrelated observations or tasks.
Factory should support many independent active runs as peers in the run
queue, dashboard, and merge queue. Parent/child runs should represent
work decomposition and dependency structure, not general scheduling.
Independent runs need dependency metadata only when one run must start
or land after another; otherwise they should execute and land
independently.

→ Resolved: Adopted in the Work model. Work Items are independent peers by default (no parent/child structure); each has its own Attempts, its own Merge Candidates, and lands independently. Demonstrated in production multiple times today including three concurrent Work Items (fargate-codex-support, per-task-timestamps, tests-auto-save-output) landing in parallel earlier and the just-completed parallel claude-auth-detection + keep-awake-toggle.
