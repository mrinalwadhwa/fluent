2026-06-19 — Factory should support launching an Attempt from a
base other than main's current HEAD.

Today, `factory work attempt` always creates the attempt's
worktree off of `main`. The attempt's base is implicitly "wherever
main is at the moment of attempt creation." This works fine for
the common case, but breaks down for:

- **Cross-writer benchmarks.** Comparing how two writers (e.g.,
  Claude vs Pi) handle the same Work Item requires both attempts
  to start from the same base. Today this is achieved through
  careful timing — launch both attempts before either merges —
  which is fragile: an unrelated commit landing on main between
  the two `factory work attempt` calls breaks the comparison.
- **Reproducing an old behaviour.** Re-running a Work Item
  against an older base (a specific tag, a recovered commit, or
  a known-good state from before a regression) requires explicit
  base selection.
- **Failed Attempt recovery.** When an Attempt fails for an
  environmental reason and you want to retry from the same base
  (not a moved-on main), you need explicit base selection.

Suggested shape: `factory work attempt <wi-id> [--base <commit-ish>]`
where the base defaults to `main` (today's behavior) and can be
overridden with any ref, tag, branch, or commit hash. The worktree
is then created off that ref.

Concrete use case that triggered this: during slice 1
(`20260618-194050-tester-deterministic-core`) we wanted to compare
Claude attempt-1 and Pi attempt-2/-3 on identical starting state.
We achieved it by launching Pi before merging Claude's candidate,
but if the merge had happened in between, or another change had
landed on main, the apples-to-apples property would have been
silently lost.

Related (not blocking): a `factory work attempt` flag to
explicitly tag the attempt's purpose (e.g., `--label "benchmark
pi-writer"`) so benchmark or experiment attempts are queryable
later.
