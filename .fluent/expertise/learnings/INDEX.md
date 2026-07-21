# Learnings Index

- [backward-compatible-serde-fields](backward-compatible-serde-fields.md) — Persisted Work model field additions and renames must preserve backward compatibility with existing on-disk JSON
- [behaviors-test-citation-sync](behaviors-test-citation-sync.md) — Test renames must update all Test: citations in behaviors.md in the same commit
- [doc-comment-attachment-in-rust](doc-comment-attachment-in-rust.md) — Inserting a function between a doc comment and its target silently re-attaches the comment to the wrong item
- [extract-logic-to-avoid-test-duplication](extract-logic-to-avoid-test-duplication.md) — Extract multi-step logic into standalone functions so integration tests call real code rather than reimplementing it
- [follow-up-journal-schema-boundary](follow-up-journal-schema-boundary.md) — Keep post-land journal interpretation in follow_up so cleanup does not become a second schema owner
- [inject-side-effects-for-testability](inject-side-effects-for-testability.md) — Side-effect functions like notify() must be injected via &dyn Fn parameters so tests can capture and assert
- [keep-architecture-doc-in-sync](keep-architecture-doc-in-sync.md) — documentation/architecture.md is a living present-tense doc; subsystem changes must update its file map and subsystem sections in the same change
- [lock-ordering-across-subsystems](lock-ordering-across-subsystems.md) — Release the queue lock before mutating the Work model; the codebase has a lock hierarchy that must not be inverted
- [needs-user-not-terminal-for-cleanup](needs-user-not-terminal-for-cleanup.md) — NeedsUser attempts are not terminal for cleanup; only Complete and Failed are reapable
- [post-land-effects-are-idempotent-and-land-safe](post-land-effects-are-idempotent-and-land-safe.md) — A completed land is durable; post-land side effects run only after merge, replay at-most-once via deterministic ids, and never undo the land on failure
- [production-lock-test-hooks](production-lock-test-hooks.md) — FLUENT_TEST lock handshakes execute in normal builds and can stall a real process while waiting for sentinel files
- [prompt-file-naming-guardrail](prompt-file-naming-guardrail.md) — Adding or renaming prompt files under prompts/ requires updating the no_legacy_prompt_files_in_prompts_dir allowlist test
- [record-divergence-in-decisions-md](record-divergence-in-decisions-md.md) — Deliberate divergences from approach.md belong in decisions.md (durable), not just progress.md (round-scoped)
- [sandbox-denials-track-template-grants](sandbox-denials-track-template-grants.md) — Handoff-only sandbox confinement depends on stripping exact shared-temp grant strings from the rendered profile
- [shell-tests-invisible-to-compiler](shell-tests-invisible-to-compiler.md) — Shell behavior tests query JSON via jq and are not caught by the compiler when serialized field names change
- [test-fixtures-use-production-state](test-fixtures-use-production-state.md) — Test fixtures must use state values that production code actually creates, not unreachable states
- [test-names-match-assertions](test-names-match-assertions.md) — Test function names must describe the behavior the test actually asserts, not what a behavior statement claims
