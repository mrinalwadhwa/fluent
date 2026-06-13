2026-06-10 — `factory cleanup --apply` should not delete a Work
Item whose only Attempt is `failed` due to a rate-limit error. The
current cleanup logic treats any non-running Attempt as terminal
and removes the whole Work Item including its durable planning
context.
→ Resolved: `27c8fbd` made rate-limit responses trigger retry
inside `Coder::run` rather than propagating as Task failure, so a
rate-limited Attempt no longer ends up `failed`. The cleanup
behavior is correct under that contract; the precondition that
made this observation relevant no longer holds.
