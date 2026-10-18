---
name: historical-reader-cargo-identity-portability
description: Share historical readers across Attempts only when Cargo's effective configuration is portable and verified
metadata:
  type: architecture
---

Pinned historical readers use a project-owned executable cache beside the
common Git directory, so linked worktrees and separate Attempts can reuse a
verified build. The Tester sandbox grants that exact cache write and executable
mapping access; preparation falls back to a fixture-local cache when the shared
path is unavailable. This cache is separate from the Attempt-scoped Cargo
dependency cache described in [[attempt-scoped-cargo-cache-shared-with-final-tester]].

Run historical builds from a fixed directory outside the calling worktree.
Cargo reads configuration from its working directory and ancestors, as well as
its effective Cargo home, so the cache identity must cover those inputs. Reject
configuration includes that cannot be verified. Give an unset `CARGO_HOME` an
isolated home instead of implicitly reading the operator's configuration.

A configuration-free Cargo home is portable: omit its absolute path from the
reader identity so separate Attempt homes can share a build. If that home has
configuration, include its path because relative configuration values may
resolve differently in another home. When changing this boundary, test reuse
across linked worktrees and distinct Cargo homes, then vary the effective
configuration and verify the returned executable and provenance, not only the
number of builds. See the broader [[test-fixtures-use-production-state]]
guidance for compatibility fixtures.
