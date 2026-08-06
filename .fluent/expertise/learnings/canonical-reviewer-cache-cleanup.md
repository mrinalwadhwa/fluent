---
name: canonical-reviewer-cache-cleanup
description: Shared reviewer caches retire only when no nonterminal Work references their candidate commit; legacy artifact-cache cleanup removes only recognized canonical directories
metadata:
  type: architecture
---

New reviewer runs use one shared cache per candidate commit under
`.fluent/work/cache/reviewers/`; reviewer artifact areas contain durable evidence
and noncanonical hook output, never private build trees. Preserve a candidate
cache while any nonterminal Work references that commit and retire it only when
no such Work remains. A terminal Review Task alone is not reclamation authority
because another Attempt may still use the same candidate cache.

Older Fluent versions may have left canonical toolchain cache directories inside
individual reviewer artifacts. Reclaim those legacy directories only after that
Review Task becomes terminal, preserve reports, logs, transcripts, and every
noncanonical hook output, and never follow cache-directory symlinks. Cleanup is
idempotent and a later serialized admission retries it without changing the
review outcome. Related:
[[serialized-reviewer-cache-admission]],
[[canonicalize-confinement-paths]].
