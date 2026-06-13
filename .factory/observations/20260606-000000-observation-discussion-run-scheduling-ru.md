2026-06-06 — Observation discussion, run scheduling, run execution, and
landing can be decoupled into separate loops. The human discussion loop
can happen whenever the human is available: review open observations,
shape briefs/behaviors/approaches/plans, and queue a batch of runs. A
run queue can then execute scheduled runs autonomously, choosing Codex,
Claude, local, or Fargate capacity to maximize available subscription and
runtime resources. The scheduler can use run priority, coder/runtime
availability, subscription limits, expected duration, reviewer load, and
dependency/network needs as inputs so scarce agent capacity is consumed
on ready work instead of waiting for the human discussion loop. Completed
runs can enter an independent merge queue that rebases, runs checks, runs
or verifies reviews, lands eligible branches, and handles conflicts. Some
runs will still end in
`needs-user`, but those should return to the human discussion queue
rather than blocking unrelated scheduled work or mergeable completed
runs.

Architecturally, separate these roles:

- Observation queue: raw ideas, incidents, and lessons. Cheap to append,
  not yet scheduled.
- Planning queue: observations that have been discussed enough to become
  briefs/behaviors/approaches/plans. Human-heavy, can happen in batches.
- Run queue: approved planned runs waiting for coder/runtime capacity.
  Machine-heavy, scheduled against Codex/Claude limits.
- Review queue: completed author work waiting for reviewers or reruns.
- Merge queue: reviewed branches waiting for rebase/check/land, with
  conflict handling and possible follow-up runs.
- Needs-user queue: runs that cannot progress autonomously, returned to
  the human discussion loop rather than blocking the run or merge queues.

The subtle win is that "human availability" and "subscription capacity"
become independently optimized resources.

Open design question: the run queue and review queue may not need to be
separate implementation queues because authoring and review form a
loop. Treat them as separate conceptual roles for now, but revisit the
boundary when implementing the workflow.

Observation sources do not have to be human-only. A live system can log
observations from telemetry, failing checks, flaky-test analysis,
production incidents, or analysis that points at a likely bug area.
Those system-generated observations can enter the same discussion and
planning flow as human notes. Similarly, the merge queue should be able
to land learnings, not only code: expertise updates, behavior mappings,
documentation corrections, and other durable project memory can be
reviewed and landed through the same queue.

The same structure should also support teams, not only one human
operator. Different people can populate observations, discuss and shape
plans, approve scheduled runs, review completed work, and operate the
merge queue independently. The queue boundaries create parallelism for
human attention as well as for agent/runtime capacity.

In that architecture, the Factory dashboard becomes the observability
surface for all of these queues: observation inflow, planning state, run
capacity, review loops, merge readiness, needs-user items, telemetry
signals, and landed learnings. It may also become the intervention
surface for humans with permission to unblock or steer the appropriate
queue.
