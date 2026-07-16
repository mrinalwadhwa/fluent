# Learnings Index

- [backward-compatible-serde-fields](backward-compatible-serde-fields.md) — New optional fields on persisted structs must use serde(default, skip_serializing_if) for backward compatibility
- [inject-side-effects-for-testability](inject-side-effects-for-testability.md) — Side-effect functions like notify() must be injected via &dyn Fn parameters so tests can capture and assert
- [needs-user-not-terminal-for-cleanup](needs-user-not-terminal-for-cleanup.md) — NeedsUser attempts are not terminal for cleanup; only Complete and Failed are reapable
