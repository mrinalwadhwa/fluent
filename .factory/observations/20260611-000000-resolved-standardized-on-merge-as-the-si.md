2026-06-11 — Resolved: standardized on `merge` as the single verb for
the whole rebase + hooks + reviewers + fast-forward operation across
the codebase. Renames: `MergeCandidateMergeStatus::Landed` → `Merged`,
`RunStatus::Landed` → `Merged`, `landed_commit` → `merged_commit`,
`finalize_landing` → `finalize_merge`, `record_candidate_landed` →
`record_candidate_merged`, `candidate_landed_commit` →
`candidate_merged_commit`, `land_worktree_run` → `merge_worktree_run`,
`worktree::land_run` → `merge_run`, `run_pre_land_hooks_for_run` →
`run_pre_merge_hooks_for_run`, `resolve_landable_run` →
`resolve_mergeable_run`, file `src/land.rs` → `src/merge.rs`. Hook
files renamed: `check-pre-land` → `check-pre-merge`, `fix-pre-land` →
`fix-pre-merge`. CLI verb: `factory land [RUN_ID]` → `factory merge
[RUN_ID]`. Test names: `fn land_*` → `fn run_merge_*` to distinguish
legacy Run model merge from Work model `work_merge_*`. The shell test
file `tests/behaviors/operations/test-land.sh` is renamed to
`test-run-merge.sh` to match the `fn run_merge_*` convention in
`tests/binary.rs`; internal function names and display strings updated
to match.
