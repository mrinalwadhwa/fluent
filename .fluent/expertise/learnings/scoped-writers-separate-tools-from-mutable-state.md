---
name: scoped-writers-separate-tools-from-mutable-state
description: Give scoped Writer retries exact validated inputs and read-only tools while keeping mutable state private
metadata:
  type: architecture
---

Usage-limit and context-repaired Writer retries use a scoped profile that
separates installed capabilities from mutable worker state. `work_task_executor`
discovers installed tools and configuration dependencies; `os` renders their read and executable-map
grants and installed-tool write denials. `writer_cache` owns the private
dependency cache. Granting that cache does not by itself make installed Rust
tools usable, and admitting those tools does not grant the operator's mutable
Cargo home.

Resolve tool paths and their symlink targets separately from `CARGO_HOME`.
Keep compiler temporary files and platform caches in private scratch through
`TMPDIR` and `CFFIXED_USER_HOME`. On macOS, tool discovery can require the
selected-developer link as well as its resolved target. Admit exact Git
configuration files in the retained Writer worktree, using Git's own conditional
include evaluator; grant no configuration parent directory. The existing
[conditional-include pattern](../security/patterns/evaluate-conditional-git-includes.md)
describes that discovery boundary.

Resolve readable assignment inputs from validated source bindings and evidence,
including recovered snapshots, as well as direct Task inputs. A snapshot can
remain authoritative without appearing in the direct input list. Production
launches and profile probes share this resolver; grant only the exact validated
files, without their parent directories. Pair a successful recovered-input read
with rejection of changed snapshot, provenance, and audit bytes so a wider
grant cannot hide a broken binding.

Prove tool usability by compiling and executing an offline Rust test through
the production retry profile and launch environment, alongside allowed source,
Git, and fresh-artifact writes. Retain denied-write probes for installed tools,
operator cache, and historical evidence. A shell launch or tool version does not
exercise compilation, linking, or executable mapping. Required host proof must
fail when real confinement is unavailable; a hermetic profile test or nested
sandbox return supplies no enforcement evidence.

For a real confinement probe, put a retained assignment input outside writable
roots. Verify that the worker can read it, cannot write it, and leaves its bytes
unchanged. Prepare the private dependency cache through the production Writer
launch boundary before probing allowed cache writes; cache preflight only
identifies its future path and does not create it.

Related: [[attempt-scoped-cargo-cache-shared-with-final-tester]],
[[strict-sandbox-capability-selection]], [[canonicalize-confinement-paths]],
[[sandbox-tests-assert-invariant-in-both-host-branches]].
