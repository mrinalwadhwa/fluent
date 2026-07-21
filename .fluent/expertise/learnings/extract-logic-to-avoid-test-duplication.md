---
name: extract-logic-to-avoid-test-duplication
description: Extract multi-step logic into standalone functions so integration tests call real code rather than reimplementing it
metadata:
  type: testing
---

When a multi-step code path is embedded inside a larger function and tests reimplement that logic inline instead of calling the real code, the test reviewer blocks. A test that reconstructs the production path (read diff, classify, apply decision) is testing its own copy — if the production code changes, the test still passes because it runs a stale duplicate.

The fix is to extract the multi-step path into a standalone function that both the production caller and the tests invoke directly. This differs from [[inject-side-effects-for-testability]] (which injects side-effect functions as parameters); this pattern restructures the code so the logic itself is reachable without the surrounding orchestration.

Symptoms of the problem:
- Test body contains ~10+ lines that mirror production logic
- Test and production code both call the same low-level git/fs functions in the same sequence
- A semantic change in production (e.g., inverting a condition) would not cause the test to fail

The test reviewer cites this principle: "Tests must exercise the code you're trying to verify. When code is hard to reach through its public interface, it's tempting to copy the logic into the test and verify the copy instead. The test passes, but it's testing itself."

**Accepted exception — model-level tests over shared primitives.** A test that drives a test-local helper reimplementing an orchestration loop (e.g. a lineage recount-and-charge loop) is acceptable *when* `behaviors.md` explicitly scopes those tests as model-level (cited against the model module such as `src/work_model.rs`), the helper exercises real production primitives (`can_authorize_descendant`, `authorize_execution`, `lineage.charged`), and real end-to-end coverage exists separately (a binary test driving the actual orchestrator). The reviewer passes this but notes the residual gap: such model tests would not catch a regression in the orchestrator's own loop. The rule still bites when there is no separate end-to-end test or the reimplemented logic *is* the thing under verification.

Related: [[inject-side-effects-for-testability]], [[test-names-match-assertions]]
