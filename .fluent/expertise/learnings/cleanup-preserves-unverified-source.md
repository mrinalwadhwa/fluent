---
name: cleanup-preserves-unverified-source
description: Assess every owned Git source before Work cleanup and retain the whole item when any source cannot be proved safe to remove
metadata:
  type: architecture
---

Terminal or abandoned Work is only a lifecycle candidate for cleanup. Before
removing any owned asset, assess every Attempt's Work branches and candidate
worktree against the target branch recorded in Writer output or its Merge
Candidate. Require a consistent, resolvable target, integrated branch tips and
candidate HEADs, a clean registered worktree, and a known ownership path.
The registered HEAD still matters when its worktree directory is missing.
If any source is unmerged, dirty, or unverifiable, retain the entire Work Item,
including earlier integrated assets, and report the affected path or branch
and reason. A failed source probe should retain that item while cleanup can
continue with independently safe items.

Keep planning read-only. Apply must reassess the complete source inventory
before deletion and check identities at each Git removal boundary. Remove
existing clean worktrees without force and delete Work branches only at their
assessed tips; remove artifacts and model records after Git assets. A source
change during removal must stop the remaining removals.

Related: [[cleanup-protects-cross-record-eligibility]],
[[test-fixtures-use-production-state]].
