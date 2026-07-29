# Test a shell orchestrator with a stateful, order-enforcing fake CLI

## Title

Drive an operator shell script that sequences `fluent` commands with a fake
`fluent` binary that records each call, rejects out-of-order calls, and mutates
a real Git repository so downstream phases observe real effects.

## Context

Some operator scripts (release harnesses, smoke gates) orchestrate a sequence
of `fluent` subcommands and must never start a model, touch the operator's home,
or reach the network in the normal test suite. A test that stubs `fluent` with a
command that always succeeds hides two failure classes:

- **Sequencing bugs** — the script calls `land` before `run`, or `run` before
  `init`, and the stub happily accepts it.
- **Effect bugs** — a later phase claims "the target is clean" or "the fixture
  test passes," but nothing ever produced the effect it inspects.

## Mechanism

- Write the fake into `<bin>/fluent` and keep durable state beside it
  (`.fake-state/stage`) so order is enforced *across* separate invocations, not
  just within one.
- On each call, append the subcommand to a log the test reads
  (`$FAKE_CMD_LOG`), then check the recorded stage and `exit 1` when the call
  arrives out of order.
- Make the fake produce **real** effects the orchestrator later inspects: create
  a candidate branch and commit the fix during `attempt run`; `git merge
  --ff-only` it during `merge-candidate land`. Downstream "target is clean" and
  "fixture test passes" checks then exercise genuine Git state.
- Gate one failure path behind an env var (`FAKE_ATTEMPT_RUN_FAILS=1`) so a
  single fake covers both the success journey and the failure-recovery case.
- For "no network, no model" guarantees, prepend a tripwire `bin` to `PATH`
  holding `curl`/`wget`/`claude`/`codex` that append to a log and exit non-zero;
  assert the log is empty after the full journey.

## Example

```bash
case "${1:-}" in
  init)
    record "init"
    [ "$(stage)" = "installed" ] || reject "init out of order (stage=$(stage))"
    set_stage "init" ;;
  attempt)
    [ "$2" = "run" ] && [ "$(stage)" = "attempt" ] || reject "run out of order"
    git checkout -q -b smoke-candidate
    printf 'hello\n' > greeting.txt
    git commit -qam "Make greeting say hello"
    git checkout -q main            # main stays unfixed until land
    set_stage "ran" ;;
esac
```
