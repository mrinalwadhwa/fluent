---
name: canonical-reviewer-cache-cleanup
description: Reviewer-cache reclamation deletes only recognized canonical cache directories, preserves all other artifacts, and retries failed cleanup at later admissions
metadata:
  type: architecture
---

Reviewer artifact areas contain both disposable build outputs and durable review
evidence. Reclaim only the canonical directories recognized by the supported
toolchains; preserve reports, logs, transcripts, and every noncanonical hook
output. Never replace this with an artifact-area deletion or with traversal that
follows cache-directory symlinks: accounting rejects a canonical symlink, and
cleanup removes a symlink itself rather than its target.

Reclaim a Review Task's managed cache only after that Task becomes terminal.
Cleanup is idempotent and does not change the review outcome when it fails;
the next cache admission retries terminal-task reclamation. This keeps evidence
available while eventually recovering cache capacity. Related:
[[serialized-reviewer-cache-admission]],
[[canonicalize-confinement-paths]].
