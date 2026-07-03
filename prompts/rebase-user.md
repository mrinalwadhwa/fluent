Rebase the candidate branch onto `{{target_branch}}`. The workspace is clean and checked out on the candidate branch when you start.

## Phase 1 — Run the rebase

1. Snapshot the candidate's changes so you can verify preservation in Phase 2:
   `git diff {{target_branch}}..HEAD > {{artifact_dir}}/pre-rebase.diff`
2. Run `git rebase {{target_branch}}`.
3. If the rebase completes without conflicts, proceed to Phase 2.
4. If the rebase stops with conflicts, work through them until the rebase completes:
   - Read each conflicting file and understand the conflict markers.
   - Resolve: keep both sides when their changes are independent. Prefer the candidate's version when both sides changed the same lines.
   - Run `git add <file>` for each resolved file.
   - Run `git rebase --continue` when every conflict at the current stopping point is resolved.
   - Repeat until the rebase completes.

## Phase 2 — Verify and clean up

Before finishing, confirm:
- The rebase has finished (no in-progress rebase state).
- The post-rebase diff (`git diff {{target_branch}}..HEAD`) contains every change from the pre-rebase snapshot at `{{artifact_dir}}/pre-rebase.diff` (context lines may shift; no candidate content should be missing).
- The workspace has no unstaged or untracked changes.

Then delete the snapshot: `rm {{artifact_dir}}/pre-rebase.diff`.

The Task completes when the rebase has finished, the workspace is clean, and the snapshot has been deleted.

## When you can't resolve

If you cannot confidently resolve a conflict — for example, semantic conflicts where both sides modified the same logic in incompatible ways — do not guess. Instead:

1. Write a diagnostic to `{{artifact_dir}}/give-up.md` describing: which files conflict, what the conflict looks like, and why you cannot resolve it.
2. Run `git rebase --abort` to restore the workspace to its pre-rebase state.
3. Stop, do not attempt further resolution.

## Rules during rebase

- You may use `git rebase -i {{target_branch}}` to clean up history during the rebase — squash fixups, reorder for clarity, or drop commits that became empty. Avoid `reword` — it opens the commit-message editor mid-rebase, which will hang. If you need to rewrite the top commit's message after the rebase, use `git commit --amend -m "<new message>"`. Every content change present in the pre-rebase candidate must be present in the post-rebase result.
- Do not run project format, lint, or test commands. Factory runs those after and needs the workspace clean when you finish.
- Only make edits necessary to resolve conflicts. Unrelated cleanup, refactoring, or fixes belong in a separate change, not this rebase.
- Do not create new branches or tags.
