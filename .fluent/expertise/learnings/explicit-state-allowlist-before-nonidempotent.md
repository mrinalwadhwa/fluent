---
name: explicit-state-allowlist-before-nonidempotent
description: Precheck gates before non-idempotent operations must use an explicit allowlist, not an else-as-safe fallback
metadata:
  type: gotcha
---

A precheck that reads durable state before a non-idempotent operation (such as
`merge-candidate land`) must enumerate the exact states that permit the operation and
route everything else through the failure contract. An `else` clause that grants the
operation permission for any unrecognized or null state can invoke the operation
twice when the system is in an unexpected or partially-completed state.

Correct pattern:
```sh
case "$status" in
  pending)   merge-candidate land ... ;;  # explicit: safe to land
  merged)    : ;;                         # explicit: already done, skip
  *)         fail_phase "$root" "land" "$land_precheck_log" "land" ;;
esac
```

Problematic pattern:
```sh
if [ "$status" = "merged" ]; then
  :  # skip
else
  merge-candidate land ...  # fires for null, unknown, or empty status too
fi
```

A JSON field that is absent or null produces an empty or literal-null value from `jq`.
If this value is not explicitly handled, the fallback branch will retry a non-idempotent
merge that may have already completed. See also [[shell-phase-failure-all-exit-paths]]
for routing the unknown-state branch, and [[json-manifest-paths-via-jq-arg]] for the
related gotcha of malformed JSON causing parse failures.
