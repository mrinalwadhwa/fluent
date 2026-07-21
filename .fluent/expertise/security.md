# Security

Fluent accepts paths from model output and durable artifacts. Validate both
their meaning and their exact lexical spelling before using them as authority,
scope, or filesystem inputs.

## Patterns

- [require-canonical-relative-path-spelling](security/patterns/require-canonical-relative-path-spelling.md) — read when an untrusted path participates in an authorization or identity decision
