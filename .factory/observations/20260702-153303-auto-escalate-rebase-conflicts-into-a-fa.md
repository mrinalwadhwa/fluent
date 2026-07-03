Auto-escalate rebase conflicts into a Factory-managed fix Work Item instead of giving up.

Today: when the rebase Coder writes give-up.md and aborts, the merge candidate fails and requires human intervention.

Proposed shape (parallel to post-merge-review-fix): on give-up, Factory auto-creates a rebase-conflict-fix-<source-attempt-id>-<timestamp> Work Item whose Brief captures the give-up diagnostic plus rebase context (target branch, source branch, pre-rebase HEAD, target HEAD, parent merge candidate reference). The fix Work Item runs the standard write → tester → review → land lifecycle. On success, the parent merge candidate updates its tip to the resolved commit and retries. On failure or depth-cap, fall back to human intervention. Depth-capped similar to FACTORY_MAX_POST_MERGE_REVIEW_FIX_DEPTH.

Key requirement — planning context must flow to the fix Work Item. The fix Writer needs the same brief/behaviors/approach/plan the parent Work Item had, not just the conflict diagnostic. Without that context the Writer can't judge which side of a conflict matches the parent Work Item's intent, and the fix devolves into guessing.

The fix Work Item must include reviewers at the end (not just Writer + Tester). Rebase conflict resolution is a substantive code change and deserves the same review discipline as any other Writer output.

Open decisions (surface when we pick this up):
- Merge candidate retry shape — update the same candidate to the new commit vs. spin a new candidate.
- Depth cap default value.
- Whether the fix Work Item runs auto-merge itself on success, or the parent re-runs its rebase step against the new commit.
- Whether give-up.md's structure should be schematized (files, hunks, semantic summary) or stay free-form prose.
- Where the fix Work Item's worktree lives (fresh Factory-managed worktree seems right).

Needs deeper code-level exploration before drafting a brief: read work_merge_executor's rebase path end-to-end, understand how MergeCandidate state transitions today, and map how planning context is currently threaded to Work Items so the fix can inherit it.

Related: post_merge_review.rs already implements the pattern for review failures. The rebase-conflict-fix mechanism should reuse the same shape and vocabulary where possible.
