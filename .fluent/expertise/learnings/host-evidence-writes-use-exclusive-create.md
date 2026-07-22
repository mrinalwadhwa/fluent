---
name: host-evidence-writes-use-exclusive-create
description: Host-owned run/handoff evidence is written with exclusive create-new and propagates every write error — never a best-effort copy
metadata:
  type: convention
---

Host-written evidence on the managed Learner run surface — preserved transcript
phases, the submitted-draft snapshot, normalization notes, and rejection
errors — is written with `OpenOptions::create_new(true)` (or `create_dir` for a
run directory), and every I/O error is propagated with context. It is never a
best-effort `let _ = std::fs::copy(...)` and never an overwrite-capable write.

The reasoning the reviewers enforce: each record is the *only* durable proof
that a phase, normalization, or rejection happened, so a swallowed error or a
silent overwrite would erase evidence with no trace. Exclusive create is also
the conflict-safety mechanism — a colliding sibling fails loudly instead of
clobbering an earlier immutable record — and a *failed* preservation must not
advance the phase/index counter it guards (a lost record cannot be allowed to
masquerade as a completed one). Run identities are likewise allocated by
scanning on-disk state and exclusive-creating the next free index, never from an
in-memory record, so a lost record cannot reuse an identity.

When adding a new kind of run or handoff evidence, follow this pattern: exclusive
create, propagate the error, and only bump any counter after the write succeeds.
Related: [[post-land-effects-are-idempotent-and-land-safe]].
