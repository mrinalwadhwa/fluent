2026-06-09 — Work Merge Candidate landing can record a false failed
state after the target branch has already fast-forwarded if managed
workspace cleanup removes the candidate workspace before the merge
driver's final status check. In `author-preflight-guidance`, `main`
advanced to `a2694ea`, merge-time reviews passed, and the managed
worktree was gone, but the merge candidate recorded `review_state:
failed` and merge status `failed` because the final `git status` check
could not `chdir` into the removed candidate workspace. The merge
executor should record landed state before cleanup and avoid checking a
workspace after it removes it; cleanup failures should warn without
turning an already-landed candidate into a failed one.
→ Resolved: b4e577b. Work Merge Candidate execution now recovers a
stored landed result if a post-landing error occurs,
`record_candidate_failure` does not overwrite a landed candidate with a
stored landed commit, and rerunning an already-landed candidate reports
the stored commit without requiring the removed candidate workspace.
Focused unit and binary tests cover the recovery helper, landed-state
failure guard, cleanup warning, and rerun-after-cleanup behavior.
