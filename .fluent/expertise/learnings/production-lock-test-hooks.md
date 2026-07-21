---
name: production-lock-test-hooks
description: FLUENT_TEST lock handshakes execute in normal builds and can stall a real process while waiting for sentinel files
metadata:
  type: gotcha
---

The real land, lineage, and follow-up operation lock paths include environment-controlled test handshakes so cross-process concurrency tests can coordinate without timing sleeps. `lineage_lock::test_phase` and `follow_up::operation_lock_test_phase` can wait indefinitely for release sentinel files; `land_lock` can write a blocked sentinel. These hooks are not compiled only under `#[cfg(test)]`, so release binaries still inspect the corresponding `FLUENT_TEST_*` variables.

Treat these variables as production-reachable behavior. Never set them in normal Fluent execution, keep their activation scoped to the expected lineage root or operation id, and account for the possibility of an indefinite stall when diagnosing a blocked lock. When changing this machinery, prefer isolating it behind a dedicated debug/test-hook boundary while retaining real-process coverage of the lock acquisition path.

Related: [[lock-ordering-across-subsystems]]
