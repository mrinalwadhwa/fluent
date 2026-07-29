---
name: exception-must-not-widen-conjunctive-invariant
description: A guard exception that relaxes one condition in an AND-chain must not replace the entire chain — every unreachable condition must remain enforced
metadata:
  type: gotcha
---

When a validation gate is a conjunction of multiple independent conditions
(e.g. `status == Complete AND review_state == Passed`), introducing an exception
for a subset of those conditions must relax only the targeted conditions, never
the whole conjunction.

The recurring mistake: the exception condition replaces the parent `if` entirely,
so all other guards are silently dropped for inputs that match the exception.
In this codebase, the host-sandbox candidate exception was written to accept
`NeedsUser`/`HostSandbox` as a full alternative to `Complete`-and-`Passed`, when
it should only relax the `Complete` requirement. The result: unreviewed, uncertain,
and failed candidates validated successfully — violating B3 and the recorded
design decision.

The correct pattern: treat each conjunct independently:

```
// Wrong — replaces the whole condition:
if (status == Complete && review_state == Passed)
    || (status == NeedsUser && pause == HostSandbox) { ... }

// Right — relaxes only the status arm:
let status_ok = status == Complete
    || (status == NeedsUser && pause == HostSandbox);
if status_ok && review_state == Passed { ... }
```

When adding a similar exception, enumerate exactly which conjuncts are relaxed and
which must remain in force, and write the condition as separate named guards so
the intent is visible to reviewers. Related: [[negative-test-cross-product-coverage]].
