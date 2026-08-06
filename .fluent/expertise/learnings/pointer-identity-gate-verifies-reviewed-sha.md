---
name: pointer-identity-gate-verifies-reviewed-sha
description: A "the reviewed commit is exactly what ships" contract is enforced by a host-side deterministic pointer-identity gate that re-reads fresh-persisted state before and after the run; sandbox confinement is preventive, not release evidence
metadata:
  type: architecture
---

When a Work Item's release contract requires the latest reviewed Writer SHA to
survive the final Tester, every review executed for that candidate, and the Learner
*unchanged*, sandbox confinement that denies expertise/candidate/Git writes is a
*preventive* control — not verification that the invariant held. A passing reviewer
from an earlier candidate may remain effective only when Fluent's host-owned
changed-domain comparison proves that the correction did not touch its domain. That
is explicit carry-forward review authority; it is not a claim that the historical
Task context names the later SHA. An end-of-run `denied_paths` (or clean final `git
status`) bail is a structurally weaker substitute that cannot prove identity: a
mutation staged by one coder invocation and reverted by a later one leaves it empty.
Reviewers require a host-owned deterministic gate instead.

The enforced mechanism lives in `work_attempt_loop`:

- Resolve the canonical reviewed Writer SHA from the latest completed Write output.
- Require that candidate worktree `HEAD`, a clean
  `git status --porcelain --untracked-files=all`, the Merge Candidate
  `candidate_commit`, and every final-round Tester and built-in-reviewer
  `review_context` all name that SHA (`check_no_expertise_pointer_identity`). A
  mismatch launches no coder.
- Select identity contexts from the final passing round only — the
  highest-numbered completed round (`final_review_round`/`task_review_round`). Every
  Tester and reviewer newly executed in that round must name the reviewed SHA.
  Earlier corrective rounds may carry older `candidate_commit`s: review admission
  must either supersede an affected role with a current review or preserve an
  unaffected pass through changed-domain invalidation. The identity gate must not
  reinterpret valid historical contexts as current-pointer mismatches, and the
  review-admission gate must not let an affected historical pass remain effective.
- Repeat the full check inside the same fresh, lock-held mutation that advances
  Learning to `HandoffPending` (`prepare_no_expertise_handoff` calls
  `check_no_expertise_pointer_identity` against the `mutate_work_item` aggregate), so
  a pointer moved during the coder run is caught and the pass/fail decision and the
  Learning write are one atomic step. Evaluating the postflight against an in-memory
  pre-run snapshot — or deciding it in one transaction and persisting the result in
  another — defeats it: the settlement must read and write the same current
  aggregate. This is the FIRST of two lock-held phases.
- The SECOND phase (`publish_no_expertise_handoff`) folds the final identity check,
  the canonical handoff publication, and the terminal settlement into ONE lock-held
  transaction accepting only this run's exact `HandoffPending` frontier. Do not write
  the handoff outside the lock and settle a precomputed result afterward — a supported
  transition could interleave and stale the handoff. Pass a validated handoff and a
  publisher closure into the finalizer and call the publisher WHILE the model lock is
  held (a test proves this by opening the model-lock file and requiring a nonblocking
  `flock` to return `WouldBlock`), then settle `Succeeded` with the digest-bearing
  reference or a typed `Failed`. A contradiction found here invokes no publisher and
  creates, replaces, or references no canonical file; an unreferenced file left by an
  earlier failed commit stays byte-for-byte unchanged and non-authoritative.
- Fail closed: any contradiction settles a relaunchable `Generic` Learning in that
  same mutation with every pointer unchanged, the contradictory value preserved as
  evidence, and no handoff published. Because every transition is a fresh
  field-level mutation, a concurrent Work-model change neither strands Learning
  `InProgress` on a stale write nor is clobbered
  ([[fresh-field-level-finalizer-preserves-concurrent-state]]).
- Serialization alone is insufficient: a pointer write queued behind the publication
  lock could run right after `Succeeded` and stale the handoff. Add a central
  reviewed-identity transition guard on the Work-model write path
  (`reject_frozen_no_expertise_identity_change`, applied before the transaction
  journal is authored) that, while a no-expertise Attempt's Learning is
  `HandoffPending` or `Succeeded`, rejects any supported write that moves its frozen
  reviewed tuple (Write task/commit, selected Merge Candidate id/commit, final-round
  Tester/reviewer context, no-expertise policy) or persists a `merged_commit` other
  than the frozen reviewed SHA. Compare against the prior durable aggregate so a
  Learning-only transition, an exact-SHA landing update, or an unrelated field still
  commits; a change while `InProgress` stays postflight-detectable and a `Failed`
  record stays repairable.
- Recover both transaction-journal outcomes honestly when the handoff is written but
  the final model commit fails: if no terminal journal became durable, keep
  `HandoffPending`, treat any unreferenced file as untrusted, rerun only the Learner,
  and atomically replace the file only after a fresh final check passes; if the
  journal became durable, the next supported read finishes it to `Succeeded` with its
  exact reference without rerunning. The failing call reports no readiness either way,
  and its returned error keeps the triggering classified fault as the discoverable
  primary with the typed `WorkModelStorageError` attached.
- Preserve the same reviewed SHA at LAND: a fresh, unmerged no-expertise+`Succeeded`
  candidate lands through an identity-preserving branch that, before any Git or model
  mutation, requires the live candidate clean at the frozen SHA and the target head an
  ancestor of it; it skips rebase and `regenerate_provenance`, runs `check-pre-merge`
  only in a disposable exact-SHA worktree (never `fix-pre-merge`), fast-forwards the
  target to that exact SHA, and persists it as `merged_commit`. A diverged target,
  dirty candidate, or mutating/failed check fails closed and requires a fresh reviewed
  Attempt rather than manufacturing an unreviewed commit.

Scope the atomicity claim honestly. The lock-held mutation makes every *supported
persisted-state* transition atomic — the postflight decision and the Learning write
read and write the same aggregate under the model lock, so no supported concurrent
Work-model change interleaves between them. It does **not** serialize candidate Git.
The postflight's final read of candidate `HEAD` and the worktree is a point-in-time
observation; an arbitrary out-of-band process that edits candidate Git after that
last read is a residual race the model lock cannot close. Eliminating it needs a
universal lock shared by every candidate-workspace Git writer — separate work, not
this gate. State the gate's guarantee at that strength; do not claim it stops every
out-of-band writer.

This identity axis is complementary to — not a duplicate of — the per-return Git
ledger ([[host-owned-git-transaction-over-untrusted-coder]]): the ledger proves the
coder mutated nothing in the managed workspace; the identity gate proves the thing
being released is exactly the reviewed commit across every pointer and review
context for newly executed final-round Tasks, while correction-aware review
invalidation proves whether older role evidence remains applicable. Confinement
mechanics are [[sandbox-denials-track-template-grants]]; the
fail-closed posture (Fargate refused before any side effect, `--no-sandbox` ignored
for the trusted-write mode) follows [[config-fails-open-only-for-diagnostics]].
