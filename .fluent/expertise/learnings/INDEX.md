# Learnings Index

- [backward-compatible-serde-fields](backward-compatible-serde-fields.md) — Persisted Work model field additions and renames must preserve backward compatibility with existing on-disk JSON
- [behaviors-test-citation-sync](behaviors-test-citation-sync.md) — Test renames must update all Test: citations in behaviors.md in the same commit
- [doc-comment-attachment-in-rust](doc-comment-attachment-in-rust.md) — Inserting a function between a doc comment and its target silently re-attaches the comment to the wrong item
- [extract-logic-to-avoid-test-duplication](extract-logic-to-avoid-test-duplication.md) — Extract multi-step logic into standalone functions so integration tests call real code rather than reimplementing it
- [inject-side-effects-for-testability](inject-side-effects-for-testability.md) — Side-effect functions like notify() must be injected via &dyn Fn parameters so tests can capture and assert
- [needs-user-not-terminal-for-cleanup](needs-user-not-terminal-for-cleanup.md) — NeedsUser attempts are not terminal for cleanup; only Complete and Failed are reapable
- [shell-tests-invisible-to-compiler](shell-tests-invisible-to-compiler.md) — Shell behavior tests query JSON via jq and are not caught by the compiler when serialized field names change
- [test-fixtures-use-production-state](test-fixtures-use-production-state.md) — Test fixtures must use state values that production code actually creates, not unreachable states
- [test-names-match-assertions](test-names-match-assertions.md) — Test function names must describe the behavior the test actually asserts, not what a behavior statement claims
