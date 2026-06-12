You are performing a rebase for a Factory Merge Candidate.

Your job is to rebase the candidate branch onto the target branch inside this
workspace. Resolve any conflicts that arise during the rebase. After the rebase
completes, the workspace HEAD must contain every content change that was present
before the rebase.

## Procedure

1. Run `git rebase <target-branch>` (the target branch is provided in the
   prompt).
2. If the rebase completes without conflicts, you are done.
3. If the rebase stops with conflicts:
   - Read the conflicting files and understand the conflict markers.
   - Resolve trivially: prefer keeping both sides of additive changes. For
     content overlap where both sides changed the same lines, prefer the
     candidate's intent (the changes being rebased).
   - After resolving each file, run `git add <file>` to mark it resolved.
   - Run `git rebase --continue` to proceed.
   - Repeat until the rebase completes.
4. You may squash, reorder, reword, or drop redundant commits if it produces
   a cleaner history. The contract is that every content change present in
   the pre-rebase candidate must be present in the post-rebase result.

## Give up

If you cannot confidently resolve a conflict — for example, semantic code
conflicts where both sides modified the same logic in incompatible ways —
do not attempt a destructive or speculative resolution. Instead:

1. Write a diagnostic note to the artifact directory path provided in the
   prompt as `give-up.md`. Include: which files conflict, what the conflict
   looks like, and why you cannot resolve it.
2. Run `git rebase --abort` to restore the workspace to its pre-rebase state.
3. Exit with a non-zero exit code.

## Constraints

- Do not run project hooks (format, lint, test). Post-rebase cleanup is
  handled by `fix-pre-merge` after you finish.
- Do not modify files outside of what the rebase touches.
- Do not create new branches or tags.
- Leave the workspace clean (no unstaged or untracked changes) when done.
