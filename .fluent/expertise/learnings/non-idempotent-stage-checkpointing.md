---
name: non-idempotent-stage-checkpointing
description: Multi-step shell workflows with non-idempotent commands need per-stage checkpoints for safe resume
metadata:
  type: architecture
---

A shell workflow that calls non-idempotent CLI operations (init, work-item create, attempt
create) in sequence cannot simply restart from the beginning on failure: the second
invocation of a creation command will be rejected because the object already exists.

The pattern is:
1. Track a `run_stage` value in the durable manifest (e.g., `installed`, `init`,
   `work-item`, `attempt`).
2. After each non-idempotent step succeeds, write the checkpoint atomically.
3. On resume, read `run_stage` and skip all stages up to and including the last completed
   one.
4. Only the stages that cannot have succeeded (those after the last checkpoint) are
   re-executed.

Tests for this contract must verify end-to-end: inject a failure after one or more stages
complete, then clear the failure and execute the printed resume command, then assert that
each creation command ran exactly once total (not twice). See also
[[next-action-guidance-is-an-executable-interface]] for the parallel Rust-side requirement
that the emitted command shape is asserted by executing it.

This pattern differs from [[reserved-phase-terminal-finalizer]] (the Rust finalizer
pattern) in that there is no single lock-held finalizer; instead, each stage is its own
mini-checkpoint within a longer phase.
