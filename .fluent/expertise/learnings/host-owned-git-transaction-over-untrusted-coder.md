---
name: host-owned-git-transaction-over-untrusted-coder
description: When a host phase drives an untrusted coder that can rewrite the managed workspace across multiple invocations, own a Git transaction — cumulative per-return ledger, host-authored canonical squash, pointers only after clean verify — never trust the coder's final HEAD
metadata:
  type: architecture
---

A host phase that repeatedly invokes an untrusted coder over one managed Git
workspace (the pre-land Learner: an initial invocation plus a bounded schema-repair
loop) cannot reason from the coder's final `HEAD` alone. The coder can commit,
stage, leave untracked files, and — critically — a *later* invocation can
`reset --hard` an earlier out-of-bounds commit out of reachable history, so
final-state accounting is unsound. The host owns a transaction instead:

- **Fail closed on entry.** Verify the workspace is at the exact baseline `HEAD`
  with a clean index, worktree, and untracked set before launching any coder. A
  dirty entry launches nothing, moves no Write/Merge-Candidate pointer, and
  settles a relaunchable failed record.
- **Pin every return before inspecting it.** Immediately after each coder
  invocation returns — before its result is inspected, its draft published, or the
  next invocation launches — capture that return's reachable per-commit paths plus
  staged, unstaged, and untracked paths into a cumulative ledger. Also capture the
  final pre-normalization state as one more snapshot. Classify the *union*, so an
  out-of-bounds effect a later reset erased from reachable history still rejects the
  whole result. Reject any snapshot whose `HEAD` is not an unambiguous linear
  descendant of the baseline (every intervening commit has exactly one parent).
- **Author the canonical result yourself.** Do not adopt the coder's commit shape.
  `reset --mixed` to the baseline, stage everything, and author exactly one squash
  commit whose sole parent is the baseline, folding committed + staged + unstaged +
  untracked (including deletions). A result with no net delta retains the baseline
  and creates no empty commit.
- **Move pointers last, roll back on any failure.** Advance the Write output and
  Merge Candidate only after the canonical result verifies exactly clean; write the
  handoff last. Any pre-acceptance failure runs one restoring rollback
  (`reset --hard` + `clean -fd`, then prove baseline `HEAD` and empty status) — the
  clean-entry precondition is what makes reset-and-clean safe.

The classification pass runs on lossless bytes — see
[[git-path-confinement-lossless-and-component-aware]]. Pointer-after-artifact
ordering and the typed rollback-vs-primary composition follow the project's
finalizer doctrine: [[reserved-phase-terminal-finalizer]],
[[compose-typed-failure-precedence]]. This host-owned pre-land path is distinct
from the post-land handoff-only confinement, which still discards any mutation on a
disposable clone.
