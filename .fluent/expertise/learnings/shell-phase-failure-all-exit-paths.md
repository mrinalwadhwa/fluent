---
name: shell-phase-failure-all-exit-paths
description: Every exit path in a shell multi-phase workflow must route through fail_phase, not bare set -e
metadata:
  type: convention
---

A shell script that implements a multi-phase operator workflow must route every failure
through a `fail_phase` (or equivalent) wrapper that names the failed phase, records the
relevant log path, and prints the exact resume command. Bare `set -e` exits that escape
this wrapper produce opaque failures: the operator receives no phase name, no log to
inspect, and no actionable resume command.

The contract applies to all failure modes across all phases:
- Directory creation, fixture seeding, and every setup step before the first checkpoint
- External tool invocations (installer, init, create, run)
- Manifest reads, writes, and checkpoint updates
- Evidence copies and post-operation verification steps
- Final marker removal and phase advancement

Initialization of the durable phase log and the incomplete marker must happen before the
first fallible operation in a phase, so that a crash during setup still leaves a
recoverable trace. See also [[atomic-manifest-within-smoke-root]] for the checkpoint
write pattern and [[non-idempotent-stage-checkpointing]] for resume-safe workflows.
