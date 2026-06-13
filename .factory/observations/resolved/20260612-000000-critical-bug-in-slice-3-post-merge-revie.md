2026-06-12 — Critical bug in slice-3 post-merge review:
`SourceCheckoutReviewGuard::finish()` (in
`src/work_task_executor.rs:566`) restores `.factory/` state to a
snapshot taken at review start. For background post-merge reviews,
this WIPES any Work Item state the user creates during the review
window.

Reproduction: after merging `optional-attempt-merge-candidate-ids`,
the post-merge review fired its detached child. The child slept 60s,
then ran a review-only Attempt against the merged commit. The
SourceCheckoutReviewGuard's begin captured the project state. During
the review, I ran `factory work create reviewer-warm-build-cache`
which wrote `.factory/work/items/reviewer-warm-build-cache.json`.
When the post-merge review finished, the guard's
`restore_non_factory_worktree_changes` + protected-factory-file
restoration reverted .factory state, deleting the new Work Item file.
`factory work attempt run` on the now-missing Work Item kept running
its detached coder against a phantom Work Item.

Root cause: the guard was designed for the interactive
`factory work review-codebase` case — the user is at the keyboard,
expects their source untouched. It's misapplied to the background
post-merge review, where the user is concurrently doing legitimate
work and Factory itself writes new state (the synthetic Work Item,
auto-created post-merge-review-fix Work Items).

Fixes to consider:

1. Differentiate by AttemptKind / origin: post-merge reviews use a
   non-restoring guard variant that only checks "did source HEAD
   move during the review" (stale → mark accordingly) without
   restoring file state. Interactive review-only Attempts keep
   today's restorative guard.
2. Run post-merge reviews in a separate worktree (clone main HEAD,
   review there, never touch the user's primary checkout). Cleaner
   isolation but uses disk + adds setup cost.
3. Narrow the protected-file snapshot to source files only, never
   `.factory/`. Factory state is mutable by Factory itself; the
   guard shouldn't snapshot+restore it.

(2) is probably the cleanest long-term answer. (3) is the smallest
patch. (1) is the most flexible. Any of these blocks future
post-merge-review-fix auto-creation from working safely.

Related: the post-merge review also failed to auto-create the
post-merge-review-fix Work Item even though the documentation
reviewer reported `Verdict: fail` (legitimate finding: duplicate
`Test:` lines around behaviors.md:783-788). The `review_one`
function in `src/post_merge_review.rs:295` filters tasks by
`status == TaskStatus::Complete`. When the source guard wipes state
mid-review, the orchestrating Attempt status flips to `failed`,
peer reviewers get stuck in `planned`, and only the first-completed
reviewer's task ends as `failed` rather than `complete`. None pass
the filter, no findings collected, no forward-fix created.

→ Resolved: Resolved by Work Item post-merge-review-guard-fix at 36809a4. PostMergeSourceGuard now differs from SourceCheckoutReviewGuard and does not restore .factory/ state.
