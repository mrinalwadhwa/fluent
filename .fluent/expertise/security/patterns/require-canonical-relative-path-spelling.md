# Require canonical relative path spelling

## Context

Use this pattern when an untrusted path determines applicable instructions,
authorization, scope, or a stable identity. `Path::components()` normalizes
some lexical aliases, so checking only that every yielded component is normal
does not prove the original spelling was canonical.

## Mechanism

Reject an empty or absolute path. Rebuild a `PathBuf` from each component while
accepting only `Component::Normal`, then compare the rebuilt `OsStr` with the
original `OsStr`. Exact comparison rejects current/parent components, repeated
separators, and trailing separators instead of silently treating their aliases
as the authorized path.

## Example

The corrective host gate accepts `src/retry.rs` but rejects
`src/./retry.rs`, `src//retry.rs`, `src/retry.rs/`, and
`src/../src/retry.rs`. It then resolves the closest committed `AGENTS.md` for
the one canonical target spelling.
